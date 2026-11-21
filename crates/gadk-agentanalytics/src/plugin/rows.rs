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

//! Rows of the analytics plugin: `build_row`, `new_event_id`, `add_content`,
//! `new_parts_collector`, `json_column`, `attributes` (with the `otel` section), `adk_envelope`,
//! `add_source_event`, `put_non_empty`, `parse_node_path`, `segment_run_id`, `derive_scope`,
//! `session_metadata`, `state_snapshot`, `llm_request_attributes`, `llm_config`,
//! `tool_declarations`, `llm_request_content`, `build_llm_request_content`,
//! `build_llm_response_content`, `llm_response_attributes`, `cache_type`, `agent_starting_data`,
//! agent config, `identity_of`, context path, `runner_agent_of`.
//!
//! The agent config is read with `gadk_engine::llm::llm_agent_config` and
//! `AgentState.agent_type`; the context path is `Context::path()`.
//!
//! Typed values in the `attributes` maps (schemas, `f32` settings, label maps, metadata structs)
//! are their sanitized JSON trees ([`go_value`]), so the JSON written for them matches ADK's (an
//! `f32` keeps its 32-bit text).

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex, PoisonError};

use chrono::Utc;
use gadk_agent::{Agent, go_quote, tool_from_request};
use gadk_engine::telemetry::d6::SpanIds;
use gadk_genai::{Content, GenerateContentConfig, GenerateContentResponseUsageMetadata};
use gadk_model::{LlmRequest, LlmResponse};
use gadk_session::{Event, ReadonlyState};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

use super::version::adk_version;
use super::{
    ADK_ENVELOPE_SCHEMA_VERSION, AnalyticsPlugin, EventData, Payload, SCOPE_KIND_FUNCTION_CALL,
    SCOPE_KIND_NODE_RUN,
};
use crate::batch_processor::BatchProcessor;
use crate::formatter::{
    ContentSummaryBuilder, MessageSummary, PartsCollector, PartsOptions,
    UNSUPPORTED_OBJ_PLACEHOLDER, sanitize_and_truncate_text, sanitize_value, smart_truncate_with,
    truncate_string,
};
use crate::redaction::{
    COLUMN_ATTRIBUTES, COLUMN_CONTENT, COLUMN_CONTENT_PARTS, COLUMN_LATENCY_MS,
    FORMATTING_FAILED_PLACEHOLDER, apply_content_formatter, redact_error_message,
};
use crate::row::{QueuedRow, Row, RowValue, from_json_map};
use crate::schema::{EVENT_ID_FIELD, TIMESTAMP_FIELD};
use crate::trace::{CallbackCtx, Identity, Latency};

/// Gives back the upload bytes `add_content` reserved if building the row panics before it is
/// marked built.
struct ReleaseUnlessBuilt<'a> {
    processor: Option<&'a BatchProcessor>,
    row: QueuedRow,
    built: bool,
}

impl Drop for ReleaseUnlessBuilt<'_> {
    fn drop(&mut self) {
        if !self.built
            && let Some(p) = self.processor
        {
            p.release_row_uploads(&mut self.row);
        }
    }
}

impl AnalyticsPlugin {
    /// The row of an event (identity, trace and span IDs, timestamp, status, error, content via
    /// `add_content`, attributes, latency), with its upload side channels in the `QueuedRow`.
    pub(crate) fn build_row(
        &self,
        ctx: &CallbackCtx,
        id: &Identity,
        event_type: &str,
        d: &mut EventData,
    ) -> QueuedRow {
        let event_id = new_event_id();
        let row_ts = d.timestamp.or(d.span.timestamp).unwrap_or_else(Utc::now);
        let mut row = Row::new();
        row.insert(TIMESTAMP_FIELD.to_owned(), RowValue::Timestamp(row_ts));
        row.insert(EVENT_ID_FIELD.to_owned(), RowValue::from(event_id.as_str()));
        row.insert("event_type".to_owned(), RowValue::from(event_type));
        row.insert(
            "invocation_id".to_owned(),
            RowValue::from(id.invocation_id.as_str()),
        );
        row.insert("user_id".to_owned(), RowValue::from(id.user_id.as_str()));
        let mut g = ReleaseUnlessBuilt {
            processor: self.processor.as_deref(),
            row: QueuedRow::new(row),
            built: false,
        };
        let mut agent_name = id.agent_name.clone();
        if agent_name.is_empty()
            && let Some(ev) = &d.source_event
        {
            agent_name = ev.author.clone();
        }
        put_non_empty_row(&mut g.row.row, "agent", &agent_name);
        put_non_empty_row(&mut g.row.row, "session_id", &id.session_id);

        put_non_empty_row(&mut g.row.row, "trace_id", &d.span.trace_id);
        put_non_empty_row(&mut g.row.row, "span_id", &d.span.span_id);
        put_non_empty_row(&mut g.row.row, "parent_span_id", &d.span.parent_span_id);
        if let Some(latency) = d.span.latency
            && !self.denied(COLUMN_LATENCY_MS)
        {
            // The batch processor writes JSON columns from their JSON text, so the latency
            // object is marshaled here, exactly once.
            if let Some(text) = go_json_text(&Value::Object(latency_object(latency))) {
                g.row
                    .row
                    .insert(COLUMN_LATENCY_MS.to_owned(), RowValue::Str(text));
            }
        }
        // The ambient span feeds trace_id (via span_fn) and attributes.otel.
        let span = ctx.span;

        let mut truncated = self.add_content(&mut g.row, event_type, d);
        let mut attrs_truncated = false;
        if !self.denied(COLUMN_ATTRIBUTES) {
            let (attrs_map, meta_trunc) = self.attributes(&event_id, id, &agent_name, span, d);
            let (attrs, t) = self.json_column(&Value::Object(attrs_map.unwrap_or_default()));
            g.row
                .row
                .insert(COLUMN_ATTRIBUTES.to_owned(), RowValue::Str(attrs));
            attrs_truncated = t || meta_trunc;
            truncated = truncated || attrs_truncated;
        }

        let status = if d.failed { "ERROR" } else { "OK" };
        g.row
            .row
            .insert("status".to_owned(), RowValue::from(status));
        let mut err_msg_truncated = false;
        if !d.err_msg.is_empty() {
            let mut err_msg = d.err_msg.clone();
            if self.config.redact_sensitive_keys() {
                err_msg = redact_error_message(&err_msg);
            }
            let (msg, t) = truncate_string(&err_msg, self.config.max_content_length());
            g.row
                .row
                .insert("error_message".to_owned(), RowValue::Str(msg));
            err_msg_truncated = t;
            truncated = truncated || t;
        }
        if let Some(builder) = &g.row.summary_builder {
            let mut b = builder.lock().unwrap_or_else(PoisonError::into_inner);
            b.other_truncated = b.other_truncated || attrs_truncated || err_msg_truncated;
        }
        g.row
            .row
            .insert("is_truncated".to_owned(), RowValue::Bool(truncated));
        g.built = true;
        std::mem::take(&mut g.row)
    }

    /// Fills the `content` / `content_parts` columns; reports truncation.
    ///
    /// Everything that goes into `row` is detached from the caller's messages; payloads to
    /// offload travel with the row as copies in `QueuedRow::raw_parts`. Synthetic events such
    /// as `AGENT_RESPONSE` that pre-populate `d.parts` when no content formatter is set copy
    /// those entries directly.
    pub(crate) fn add_content(
        &self,
        row: &mut QueuedRow,
        event_type: &str,
        d: &mut EventData,
    ) -> bool {
        let content_denied = self.denied(COLUMN_CONTENT);
        let parts_denied = self.denied(COLUMN_CONTENT_PARTS);
        let Some(raw) = d.content.as_ref() else {
            return false;
        };
        if payload_is_nil(raw) || (content_denied && parts_denied) {
            return false;
        }
        // Apply the content formatter: `formatted` is `None` when the payload is unchanged.
        let mut formatted: Option<Payload> = None;
        if let Some(formatter) = &self.config.content_formatter {
            let input = payload_json(raw);
            let (out, err_note, failed) = apply_content_formatter(
                Some(formatter),
                self.config.logger.as_deref(),
                &input,
                event_type,
                self.config.debug_content_formatter_errors,
            );
            if failed {
                d.attributes
                    .insert("formatter_failed".to_owned(), Value::Bool(true));
                if !err_note.is_empty() {
                    if !d.err_msg.is_empty() {
                        d.err_msg.push_str("; ");
                    }
                    d.err_msg.push_str(&err_note);
                }
                if let Some(p) = &self.processor {
                    p.record_formatter_failure_drop();
                }
                if !content_denied {
                    row.row.insert(
                        COLUMN_CONTENT.to_owned(),
                        RowValue::Str(go_quote(FORMATTING_FAILED_PLACEHOLDER)),
                    );
                }
                return false;
            }
            if out != input {
                formatted = Some(typed_like(raw, out));
            }
        }
        let payload = formatted.as_ref().unwrap_or(raw);
        if payload_is_nil(payload) {
            return false;
        }

        let max_len = self.config.max_content_length();
        let redact = self.config.redact_sensitive_keys();
        // The guard moves the queued uploads onto the row however this returns, a panic
        // included, so the caller's guard releases their reserved bytes.
        let mut g = PendingPartsGuard {
            parts: (!parts_denied && self.config.log_multi_modal_content())
                .then(|| self.new_parts_collector(max_len)),
            row,
        };
        // By payload type: `(content, content_truncated, pre_formatted, builder)`.
        let (content, content_truncated, pre_formatted, builder) = match payload {
            Payload::Content(c) => {
                if let Some(parts) = g.parts.as_mut() {
                    parts.add_content(Some(c), 0);
                }
                let mut b = ContentSummaryBuilder {
                    content_denied,
                    max_len,
                    redact,
                    messages: vec![MessageSummary::new(Some(c), 0, max_len, redact)],
                    ..ContentSummaryBuilder::default()
                };
                if let Some(parts) = g.parts.as_ref() {
                    omit_pending_text(&mut b, parts);
                }
                let (built, t) = b.build_content();
                b.needs_rebuild = false;
                (Some(Value::Object(built)), t, true, Some(b))
            }
            Payload::LlmRequest(req) => {
                let (built, t, b) = build_llm_request_content(
                    req,
                    max_len,
                    redact,
                    content_denied,
                    g.parts.as_mut(),
                );
                (built.map(Value::Object), t, true, b)
            }
            Payload::LlmResponse(res) => {
                let (built, t, b) = build_llm_response_content(
                    res,
                    max_len,
                    redact,
                    content_denied,
                    g.parts.as_mut(),
                );
                (built.map(Value::Object), t, true, b)
            }
            Payload::Json(v) => {
                if let Some(parts) = g.parts.as_mut()
                    && !d.parts.is_empty()
                    && self.config.content_formatter.is_none()
                {
                    parts.parts.extend(d.parts.iter().cloned());
                    parts.truncated = parts.truncated || d.parts_truncated;
                    parts.non_upload_truncated = parts.non_upload_truncated || d.parts_truncated;
                }
                (Some(v.clone()), d.content_truncated, false, None)
            }
        };

        let mut truncated = false;
        if !content_denied && let Some(content) = content.filter(|c| !c.is_null()) {
            if pre_formatted {
                match go_json_text(&content) {
                    Some(text) => {
                        g.row
                            .row
                            .insert(COLUMN_CONTENT.to_owned(), RowValue::Str(text));
                        truncated = content_truncated;
                    }
                    None => {
                        g.row.row.insert(
                            COLUMN_CONTENT.to_owned(),
                            RowValue::Str(go_quote(UNSUPPORTED_OBJ_PLACEHOLDER)),
                        );
                        truncated = true;
                    }
                }
            } else {
                let (s, t) = self.json_column(&content);
                g.row
                    .row
                    .insert(COLUMN_CONTENT.to_owned(), RowValue::Str(s));
                truncated = t || content_truncated;
            }
        }
        if let Some(parts) = g.parts.as_ref()
            && !parts.parts.is_empty()
        {
            g.row.row.insert(
                COLUMN_CONTENT_PARTS.to_owned(),
                RowValue::Parts(parts.parts.iter().map(from_json_map).collect()),
            );
            truncated = truncated || parts.truncated;
            if !parts.pending.is_empty()
                && let Some(mut b) = builder
                && parts.has_pending_text()
            {
                b.other_truncated = parts.non_upload_truncated;
                g.row.summary_builder = Some(Arc::new(Mutex::new(b)));
            }
        }
        truncated
    }

    /// A parts collector for `max_len`: offloading to Cloud Storage is on if the plugin has an
    /// offloader.
    pub(crate) fn new_parts_collector(&self, max_len: i64) -> PartsCollector {
        let mut opts = PartsOptions {
            max_len,
            redact: self.config.redact_sensitive_keys(),
            connection_id: self.config.connection_id.clone(),
            ..PartsOptions::default()
        };
        if self.config.gcs_offloader.is_some() {
            opts.offload = true;
            opts.inline_limit = self.config.max_inline_text_size;
            if let Some(p) = &self.processor {
                let p = Arc::clone(p);
                opts.reserve_bytes = Some(Arc::new(move |n| p.try_reserve_upload_bytes(n)));
            }
        }
        PartsCollector::new(opts)
    }

    /// `(text, truncated)` — `v` as the JSON text of a JSON column, with truncation and
    /// sensitive-key redaction.
    pub(crate) fn json_column(&self, v: &Value) -> (String, bool) {
        match smart_truncate_with(
            v,
            self.config.max_content_length(),
            self.config.redact_sensitive_keys(),
        ) {
            Ok((b, truncated)) => match String::from_utf8(b) {
                Ok(text) => (text, truncated),
                Err(_) => (go_quote(UNSUPPORTED_OBJ_PLACEHOLDER), true),
            },
            Err(_) => (go_quote(UNSUPPORTED_OBJ_PLACEHOLDER), true),
        }
    }

    /// The `attributes` object — the event-specific attributes followed by the common ones — and
    /// whether scrubbing `custom_metadata` lost content (always false). `span` is the ADK span
    /// the callback runs in. Never `None` (the object always has `adk`).
    pub(crate) fn attributes(
        &self,
        event_id: &str,
        id: &Identity,
        agent_name: &str,
        span: Option<SpanIds>,
        d: &mut EventData,
    ) -> (Option<Map<String, Value>>, bool) {
        let mut attrs = d.attributes.clone();
        attrs.insert(
            "adk".to_owned(),
            Value::Object(self.adk_envelope(event_id, id, agent_name, d)),
        );
        put_non_empty(&mut attrs, "root_agent_name", &d.span.root_agent_name);
        if !self.config.custom_tags.is_empty() {
            attrs.insert(
                "custom_tags".to_owned(),
                Value::Object(self.config.custom_tags.clone()),
            );
        }
        if self.config.log_session_metadata() {
            attrs.insert(
                "session_metadata".to_owned(),
                Value::Object(session_metadata(id)),
            );
        }
        if self.config.enable_otel_correlation
            && let Some(span) = span.filter(SpanIds::is_valid)
        {
            let mut otel = Map::new();
            otel.insert(
                "span_id".to_owned(),
                Value::from(crate::trace::hex(&span.span_id)),
            );
            otel.insert(
                "trace_id".to_owned(),
                Value::from(crate::trace::hex(&span.trace_id)),
            );
            attrs.insert("otel".to_owned(), Value::Object(otel));
        }
        let meta_trunc = false;
        if let Some(md) = d
            .custom_metadata
            .as_ref()
            .and_then(|md| self.custom_metadata.apply(md))
        {
            attrs.insert("custom_metadata".to_owned(), Value::Object(md));
        }
        (Some(attrs), meta_trunc)
    }

    /// `attributes.adk`, which identifies the ADK objects a row belongs to.
    pub(crate) fn adk_envelope(
        &self,
        event_id: &str,
        id: &Identity,
        agent_name: &str,
        d: &EventData,
    ) -> Map<String, Value> {
        let mut adk = Map::new();
        adk.insert(
            "schema_version".to_owned(),
            Value::from(ADK_ENVELOPE_SCHEMA_VERSION),
        );
        adk.insert("event_id".to_owned(), Value::from(event_id));
        put_non_empty(&mut adk, "adk_version", adk_version());
        put_non_empty(&mut adk, "app_name", &id.app_name);
        put_non_empty(&mut adk, "invocation_id", &id.invocation_id);
        put_non_empty(&mut adk, "function_call_id", &d.function_call_id);
        if self.config.log_session_metadata() {
            put_non_empty(&mut adk, "session_id", &id.session_id);
            put_non_empty(&mut adk, "user_id", &id.user_id);
        }
        let author = if d.author.is_empty() {
            agent_name
        } else {
            d.author.as_str()
        };
        put_non_empty(&mut adk, "author", author);
        if let Some(ev) = &d.source_event {
            add_source_event(&mut adk, ev);
        }
        for (k, v) in &d.adk_extras {
            if !adk.contains_key(k) {
                adk.insert(k.clone(), v.clone());
            }
        }
        adk
    }
}

/// A random UUID as 32 lowercase hex digits (adk-python's `uuid4().hex`).
pub(crate) fn new_event_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// Adds the identifying fields of the session event `ev` to `adk`.
pub(crate) fn add_source_event(adk: &mut Map<String, Value>, ev: &Event) {
    put_non_empty(adk, "source_event_id", &ev.id);
    put_non_empty(adk, "branch", &ev.branch);
    if let Some(ni) = &ev.node_info {
        let (run_id, parent_run_id) = parse_node_path(&ni.path);
        let mut node = Map::new();
        node.insert("path".to_owned(), Value::from(ni.path.as_str()));
        node.insert("run_id".to_owned(), Value::from(run_id));
        node.insert("parent_run_id".to_owned(), parent_run_id);
        if ni.message_as_output {
            node.insert("message_as_output".to_owned(), Value::Bool(true));
        }
        if !ni.output_for.is_empty() {
            node.insert("output_for".to_owned(), go_value(&ni.output_for));
        }
        adk.insert("node".to_owned(), Value::Object(node));
    }
    if !ev.isolation_scope.is_empty() {
        adk.insert(
            "isolation_scope".to_owned(),
            Value::from(ev.isolation_scope.as_str()),
        );
        adk.insert(
            "scope".to_owned(),
            Value::Object(derive_scope(&ev.isolation_scope)),
        );
    }
    if !ev.routes.is_empty() {
        adk.insert("route".to_owned(), go_value(&ev.routes));
    }
    if let Some(delta) = ev.actions.artifact_delta.as_ref().filter(|d| !d.is_empty()) {
        adk.insert("artifact_delta".to_owned(), go_value(delta));
    }
    if ev.actions.skip_summarization {
        adk.insert("skip_summarization".to_owned(), Value::Bool(true));
    }
    if ev.actions.escalate {
        adk.insert("escalate".to_owned(), Value::Bool(true));
    }
}

/// Puts `value` under `key` unless it is empty.
pub(crate) fn put_non_empty(m: &mut Map<String, Value>, key: &str, value: &str) {
    if !value.is_empty() {
        m.insert(key.to_owned(), Value::from(value));
    }
}

/// Puts `value` under `key` in a [`Row`] unless it is empty.
fn put_non_empty_row(row: &mut Row, key: &str, value: &str) {
    if !value.is_empty() {
        row.insert(key.to_owned(), RowValue::from(value));
    }
}

/// The run ID of the last segment of a node path (`parent/name@run_id`) and the run ID of the
/// segment before it (`null` when there is no parent segment or it has no run ID), as
/// adk-python's `NodeInfo` does.
pub(crate) fn parse_node_path(path: &str) -> (String, Value) {
    if path.is_empty() {
        return (String::new(), Value::Null);
    }
    let segments: Vec<&str> = path.split('/').collect();
    let last = segments.len() - 1;
    let run_id = segment_run_id(segments[last]).unwrap_or_default();
    if segments.len() < 2 {
        return (run_id, Value::Null);
    }
    match segment_run_id(segments[last - 1]) {
        Some(parent) => (run_id, Value::from(parent)),
        None => (run_id, Value::Null),
    }
}

/// The text after the last `@` of `segment`, if any.
pub(crate) fn segment_run_id(segment: &str) -> Option<String> {
    segment.rfind('@').map(|i| segment[i + 1..].to_owned())
}

/// A scope whose last path segment has the node form `name@run_id` is a node run, any other
/// scope a function call ID (as adk-python classifies them).
pub(crate) fn derive_scope(scope: &str) -> Map<String, Value> {
    let last = scope.rfind('/').map_or(scope, |i| &scope[i + 1..]);
    let kind = if last.contains('@') {
        SCOPE_KIND_NODE_RUN
    } else {
        SCOPE_KIND_FUNCTION_CALL
    };
    let mut m = Map::new();
    m.insert("id".to_owned(), Value::from(scope));
    m.insert("kind".to_owned(), Value::from(kind));
    m
}

/// `attributes.session_metadata`. A state whose snapshot panics is left out.
pub(crate) fn session_metadata(id: &Identity) -> Map<String, Value> {
    let mut md = Map::new();
    md.insert("session_id".to_owned(), Value::from(id.session_id.as_str()));
    md.insert("app_name".to_owned(), Value::from(id.app_name.as_str()));
    md.insert("user_id".to_owned(), Value::from(id.user_id.as_str()));
    let state = catch_unwind(AssertUnwindSafe(|| state_snapshot(id.state.as_ref())))
        .ok()
        .flatten()
        .filter(|s| !s.is_empty());
    if let Some(state) = state {
        md.insert("state".to_owned(), Value::Object(state));
    }
    md
}

/// A snapshot of the state (`None` for no state).
pub(crate) fn state_snapshot(s: Option<&Arc<dyn ReadonlyState>>) -> Option<Map<String, Value>> {
    s.map(|s| s.all().into_iter().collect())
}

/// The model, generation settings, labels and tool declarations of `req`.
pub(crate) fn llm_request_attributes(req: &LlmRequest) -> Map<String, Value> {
    let mut attrs = Map::new();
    if !req.model.is_empty() {
        attrs.insert("model".to_owned(), Value::from(req.model.as_str()));
    }
    if let Some(cfg) = &req.config {
        if let Some(c) = llm_config(Some(cfg)).filter(|c| !c.is_empty()) {
            attrs.insert("llm_config".to_owned(), Value::Object(c));
        }
        if !cfg.labels.is_empty() {
            attrs.insert("labels".to_owned(), go_value(&cfg.labels));
        }
    }
    let tools = tool_declarations(req);
    if !tools.is_empty() {
        attrs.insert(
            "tools".to_owned(),
            Value::Array(tools.into_iter().map(Value::Object).collect()),
        );
    }
    attrs
}

/// The generation settings adk-python records, plus the thinking configuration; unset settings
/// are left out. `None` for no config.
pub(crate) fn llm_config(cfg: Option<&GenerateContentConfig>) -> Option<Map<String, Value>> {
    let cfg = cfg?;
    let mut c = Map::new();
    put_if_set(&mut c, "temperature", cfg.temperature.as_ref());
    put_if_set(&mut c, "top_p", cfg.top_p.as_ref());
    put_if_set(&mut c, "top_k", cfg.top_k.as_ref());
    if cfg.candidate_count != 0 {
        c.insert(
            "candidate_count".to_owned(),
            Value::from(cfg.candidate_count),
        );
    }
    if cfg.max_output_tokens != 0 {
        c.insert(
            "max_output_tokens".to_owned(),
            Value::from(cfg.max_output_tokens),
        );
    }
    if !cfg.stop_sequences.is_empty() {
        c.insert("stop_sequences".to_owned(), go_value(&cfg.stop_sequences));
    }
    put_if_set(&mut c, "presence_penalty", cfg.presence_penalty.as_ref());
    put_if_set(&mut c, "frequency_penalty", cfg.frequency_penalty.as_ref());
    if !cfg.response_mime_type.is_empty() {
        c.insert(
            "response_mime_type".to_owned(),
            Value::from(cfg.response_mime_type.as_str()),
        );
    }
    if let Some(schema) = &cfg.response_schema {
        c.insert("response_schema".to_owned(), go_value(schema));
    }
    put_if_set(&mut c, "seed", cfg.seed.as_ref());
    if cfg.response_logprobs {
        c.insert("response_logprobs".to_owned(), Value::Bool(true));
    }
    put_if_set(&mut c, "logprobs", cfg.logprobs.as_ref());
    if let Some(tc) = &cfg.thinking_config {
        c.insert("thinking_config".to_owned(), go_value(tc));
    }
    Some(c)
}

/// Puts `v` under `key` if it is set.
fn put_if_set<T: Serialize>(m: &mut Map<String, Value>, key: &str, v: Option<&T>) {
    if let Some(v) = v {
        m.insert(key.to_owned(), go_value(v));
    }
}

/// The tools of `req`, sorted by name, as `{name, description, parameters}` objects.
/// Descriptions and parameter schemas come from the tool or, failing that, from its function
/// declaration in the config.
pub(crate) fn tool_declarations(req: &LlmRequest) -> Vec<Map<String, Value>> {
    if req.tools.is_empty() {
        return Vec::new();
    }
    let mut config_decls = std::collections::HashMap::new();
    if let Some(cfg) = &req.config {
        for t in &cfg.tools {
            for fd in &t.function_declarations {
                if !fd.name.is_empty() {
                    config_decls.insert(fd.name.clone(), fd.clone());
                }
            }
        }
    }
    let mut keys: Vec<&String> = req.tools.keys().collect();
    keys.sort();
    let mut decls = Vec::with_capacity(keys.len());
    for key in keys {
        let mut entry = Map::new();
        entry.insert("name".to_owned(), Value::from(key.as_str()));
        let mut fd = None;
        if let Some(t) = tool_from_request(req, key) {
            let name = if t.name().is_empty() {
                key.as_str()
            } else {
                t.name()
            };
            entry.insert("name".to_owned(), Value::from(name));
            if !t.description().is_empty() {
                entry.insert("description".to_owned(), Value::from(t.description()));
            }
            fd = t.declaration();
        }
        if fd.is_none() {
            fd = config_decls.get(key.as_str()).cloned();
        }
        if let Some(fd) = fd {
            if !entry.contains_key("description") && !fd.description.is_empty() {
                entry.insert(
                    "description".to_owned(),
                    Value::from(fd.description.as_str()),
                );
            }
            if let Some(schema) = &fd.parameters_json_schema {
                entry.insert("parameters".to_owned(), go_value(schema));
            } else if let Some(schema) = &fd.parameters {
                entry.insert("parameters".to_owned(), go_value(schema));
            }
        }
        decls.push(entry);
    }
    decls
}

/// `(content, truncated)` — the role and text summary of each request message and the summary
/// of the system instruction, as adk-python builds them; `parts` (if any) collects the
/// `content_parts` of the messages, then those of the system instruction.
#[cfg(test)]
pub(crate) fn llm_request_content(
    req: &LlmRequest,
    max_len: i64,
    redact: bool,
    parts: Option<&mut PartsCollector>,
) -> (Map<String, Value>, bool) {
    let (content, truncated, _) = build_llm_request_content(req, max_len, redact, false, parts);
    (content.unwrap_or_default(), truncated)
}

/// `(content, truncated, summary builder)`. The content ordinal of a message is its index in
/// `req.contents`; that of the system instruction is `req.contents.len()`. The content is never
/// `None`.
pub(crate) fn build_llm_request_content(
    req: &LlmRequest,
    max_len: i64,
    redact: bool,
    content_denied: bool,
    mut parts: Option<&mut PartsCollector>,
) -> (
    Option<Map<String, Value>>,
    bool,
    Option<ContentSummaryBuilder>,
) {
    let mut b = ContentSummaryBuilder {
        is_llm_request: true,
        content_denied,
        max_len,
        redact,
        ..ContentSummaryBuilder::default()
    };
    for (i, c) in req.contents.iter().enumerate() {
        let role = if c.role.is_empty() {
            "unknown"
        } else {
            c.role.as_str()
        };
        let (role, t) = sanitize_and_truncate_text(role, max_len, redact);
        b.role_truncated = b.role_truncated || t;
        let mut msg = MessageSummary::new(Some(c), i, max_len, redact);
        msg.role = role;
        msg.has_role = true;
        b.messages.push(msg);
        if let Some(parts) = parts.as_deref_mut() {
            parts.add_content(Some(c), i);
        }
    }
    if let Some(si) = req
        .config
        .as_ref()
        .and_then(|c| c.system_instruction.as_ref())
    {
        let ord = req.contents.len();
        let mut msg = MessageSummary::new(Some(si), ord, max_len, redact);
        msg.is_system = true;
        b.messages.push(msg);
        if let Some(parts) = parts.as_deref_mut() {
            parts.add_content(Some(si), ord);
        }
    }
    if let Some(parts) = parts.as_deref() {
        omit_pending_text(&mut b, parts);
    }
    let (content, truncated) = b.build_content();
    b.needs_rebuild = false;
    (Some(content), truncated, Some(b))
}

/// The formatted response and the token usage, as in adk-python, plus `content_parts` when
/// `parts` is set (the text of every offloaded part leaves `content.response`). `None` content
/// when `res` carries neither.
pub(crate) fn build_llm_response_content(
    res: &LlmResponse,
    max_len: i64,
    redact: bool,
    content_denied: bool,
    parts: Option<&mut PartsCollector>,
) -> (
    Option<Map<String, Value>>,
    bool,
    Option<ContentSummaryBuilder>,
) {
    let mut b = ContentSummaryBuilder {
        is_llm_response: true,
        content_denied,
        max_len,
        redact,
        ..ContentSummaryBuilder::default()
    };
    if let Some(c) = &res.content {
        b.messages = vec![MessageSummary::new_response(Some(c), 0, max_len, redact)];
        if let Some(parts) = parts {
            parts.add_content(Some(c), 0);
            omit_pending_text(&mut b, parts);
        }
    }
    if let Some(u) = &res.usage_metadata {
        let mut usage = Map::new();
        usage.insert("prompt".to_owned(), Value::from(u.prompt_token_count));
        usage.insert(
            "completion".to_owned(),
            Value::from(u.candidates_token_count),
        );
        usage.insert("total".to_owned(), Value::from(u.total_token_count));
        if u.thoughts_token_count > 0 {
            usage.insert("thoughts".to_owned(), Value::from(u.thoughts_token_count));
        }
        if u.cached_content_token_count > 0 {
            usage.insert(
                "cached".to_owned(),
                Value::from(u.cached_content_token_count),
            );
        }
        b.usage = Some(usage);
    }
    let (content, truncated) = b.build_content();
    b.needs_rebuild = false;
    if content.is_empty() {
        return (None, false, None);
    }
    (Some(content), truncated, Some(b))
}

/// On drop (a normal return or an unwind) moves the collector's queued uploads onto the row.
struct PendingPartsGuard<'a> {
    row: &'a mut QueuedRow,
    parts: Option<PartsCollector>,
}

impl Drop for PendingPartsGuard<'_> {
    fn drop(&mut self) {
        if let Some(parts) = self.parts.as_mut()
            && !parts.pending.is_empty()
        {
            self.row.raw_parts = std::mem::take(&mut parts.pending);
        }
    }
}

/// The text of every queued text upload leaves the summary.
fn omit_pending_text(b: &mut ContentSummaryBuilder, parts: &PartsCollector) {
    for u in &parts.pending {
        if u.is_text {
            b.omit_text_from_summary(u.content_ord, u.part_idx);
        }
    }
}

/// The attributes of a model response.
pub(crate) fn llm_response_attributes(res: &LlmResponse) -> Map<String, Value> {
    let mut attrs = Map::new();
    attrs.insert("partial".to_owned(), Value::Bool(res.partial));
    attrs.insert("turn_complete".to_owned(), Value::Bool(res.turn_complete));
    attrs.insert("interrupted".to_owned(), Value::Bool(res.interrupted));
    put_non_empty(&mut attrs, "model_version", &res.model_version);
    if let Some(u) = &res.usage_metadata {
        let mut usage = Map::new();
        usage.insert(
            "prompt_token_count".to_owned(),
            Value::from(u.prompt_token_count),
        );
        usage.insert(
            "candidates_token_count".to_owned(),
            Value::from(u.candidates_token_count),
        );
        usage.insert(
            "total_token_count".to_owned(),
            Value::from(u.total_token_count),
        );
        if u.thoughts_token_count > 0 {
            usage.insert(
                "thoughts_token_count".to_owned(),
                Value::from(u.thoughts_token_count),
            );
        }
        if u.cached_content_token_count > 0 {
            usage.insert(
                "cached_content_token_count".to_owned(),
                Value::from(u.cached_content_token_count),
            );
        }
        if u.tool_use_prompt_token_count > 0 {
            usage.insert(
                "tool_use_prompt_token_count".to_owned(),
                Value::from(u.tool_use_prompt_token_count),
            );
        }
        attrs.insert("usage_metadata".to_owned(), Value::Object(usage));
    }
    if let Some(reason) = res
        .finish_reason
        .as_ref()
        .filter(|r| !r.as_str().is_empty())
    {
        attrs.insert("finish_reason".to_owned(), Value::from(reason.as_str()));
    }
    if let Some(gm) = &res.grounding_metadata {
        attrs.insert("grounding_metadata".to_owned(), go_value(gm));
    }
    if let Some(cm) = &res.citation_metadata {
        attrs.insert("citation_metadata".to_owned(), go_value(cm));
    }
    attrs.insert(
        "cache_type".to_owned(),
        Value::from(cache_type(res.usage_metadata.as_ref())),
    );
    attrs
}

/// The `prompt` attribute of model rows: the prompt version recorded for the callback's agent
/// (`{"name", "version", "label"}`, `label` omitted when the record has none); `None` without an
/// ADK context or a record.
pub(crate) fn prompt_attribute(ctx: &CallbackCtx) -> Option<Value> {
    let used = ctx.adk.as_ref()?.recorded_prompt()?;
    let mut prompt = Map::new();
    prompt.insert("name".to_owned(), Value::from(used.name));
    prompt.insert("version".to_owned(), Value::from(used.version));
    if let Some(label) = used.label {
        prompt.insert("label".to_owned(), Value::from(label));
    }
    Some(Value::Object(prompt))
}

/// `"none"` without cached tokens, `"implicit"` with them (ADK responses carry no explicit
/// context-cache metadata).
pub(crate) fn cache_type(u: Option<&GenerateContentResponseUsageMetadata>) -> String {
    match u {
        Some(u) if u.cached_content_token_count > 0 => "implicit".to_owned(),
        _ => "none".to_owned(),
    }
}

/// `(content, attributes)` of an `AGENT_STARTING` row, as adk-python: the static instruction of
/// an LLM agent as content, and its description, model, sub-agent names and, when an instruction
/// provider builds the instruction, `instruction_source: "provider"` (with no content).
pub(crate) fn agent_starting_data(a: &dyn Agent) -> (Option<Value>, Map<String, Value>) {
    let mut attrs = Map::new();
    put_non_empty(&mut attrs, "agent_description", a.description());
    let sub_agents: Vec<Value> = a
        .sub_agents()
        .iter()
        .map(|s| s.name())
        .filter(|n| !n.is_empty())
        .map(Value::from)
        .collect();
    if !sub_agents.is_empty() {
        attrs.insert("sub_agents".to_owned(), Value::Array(sub_agents));
    }
    // Only an LLM agent's own config is read (`llm_agent_config`); a user agent that wraps an
    // LLM agent is not unwrapped.
    let Some(cfg) = gadk_engine::llm::llm_agent_config(a) else {
        return (None, attrs);
    };
    if let Some(model) = &cfg.model {
        put_non_empty(&mut attrs, "model", model.name());
    }
    if cfg.instruction_provider.is_some() {
        attrs.insert("instruction_source".to_owned(), Value::from("provider"));
        return (None, attrs);
    }
    let content = (!cfg.instruction.is_empty()).then(|| Value::from(cfg.instruction.as_str()));
    (content, attrs)
}

/// The invocation fields of the callback's context (empty for a plain context). gadk's ADK
/// contexts always have an agent and a session, so no field needs a per-field panic guard.
pub(crate) fn identity_of(ctx: &CallbackCtx) -> Identity {
    let Some(c) = &ctx.adk else {
        return Identity::default();
    };
    Identity {
        agent_name: c.agent_name(),
        session_id: c.session_id().to_owned(),
        invocation_id: c.invocation_id().to_owned(),
        user_id: c.user_id().to_owned(),
        app_name: c.app_name().to_owned(),
        branch: c.branch(),
        // A fresh invocation's own path falls back to its fork's.
        path: c.context_path(),
        state: Some(c.readonly_state()),
    }
}

/// The agent of an invocation callback's context (the agent the runner runs), or `None`.
pub(crate) fn runner_agent_of(ctx: &CallbackCtx) -> Option<Arc<dyn Agent>> {
    ctx.adk.as_ref().map(|c| c.agent())
}

/// The sanitized JSON tree of the typed value `v` held in an `attributes` map (nothing
/// truncated or redacted; `json_column` does both later), so the JSON written for it matches
/// ADK's.
fn go_value<T: Serialize + ?Sized>(v: &T) -> Value {
    sanitize_value(v, -1, false).0
}

/// `v` serialized as JSON text, or `None` when it fails.
fn go_json_text(v: &Value) -> Option<String> {
    gadk_model::to_go_json(v)
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
}

/// The latency object of a row.
fn latency_object(l: Latency) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("total_ms".to_owned(), Value::from(l.total_ms));
    if let Some(ttft) = l.time_to_first_token_ms {
        m.insert("time_to_first_token_ms".to_owned(), Value::from(ttft));
    }
    m
}

/// Whether the payload is nil: only a JSON `null` is.
fn payload_is_nil(p: &Payload) -> bool {
    matches!(p, Payload::Json(Value::Null))
}

/// The JSON a content formatter receives for a payload. gadk's `ContentFormatter` maps JSON to
/// JSON, so a typed payload is passed as its JSON (`LlmRequest` as an object with `Model`,
/// `Contents` and `Config` keys, see [`llm_request_json`]) and [`typed_like`] reads a result
/// that still has the payload's shape back as that type.
fn payload_json(p: &Payload) -> Value {
    match p {
        Payload::Content(c) => serde_json::to_value(c).unwrap_or(Value::Null),
        Payload::LlmRequest(req) => llm_request_json(req),
        Payload::LlmResponse(res) => serde_json::to_value(res.as_ref()).unwrap_or(Value::Null),
        Payload::Json(v) => v.clone(),
    }
}

/// The JSON of a model request as an object with the keys `Model`, `Contents` (`null` when
/// empty) and `Config` (`null` when unset); the request's tools are not included.
fn llm_request_json(req: &LlmRequest) -> Value {
    let mut m = Map::new();
    m.insert("Model".to_owned(), Value::from(req.model.as_str()));
    let contents = if req.contents.is_empty() {
        Value::Null
    } else {
        serde_json::to_value(&req.contents).unwrap_or(Value::Null)
    };
    m.insert("Contents".to_owned(), contents);
    let config = req
        .config
        .as_ref()
        .and_then(|c| serde_json::to_value(c).ok())
        .unwrap_or(Value::Null);
    m.insert("Config".to_owned(), config);
    Value::Object(m)
}

/// The formatter's result `v` for the payload `like`: the payload's type when `v` has its
/// shape (it reads back as that type and, for `Content` / `LlmResponse`, serializes to `v`
/// again; for `LlmRequest`, an object of `Model` / `Contents` / `Config` only), else JSON.
fn typed_like(like: &Payload, v: Value) -> Payload {
    match like {
        Payload::Content(_) => match round_trip::<Content>(&v) {
            Some(c) => Payload::Content(c),
            None => Payload::Json(v),
        },
        Payload::LlmResponse(_) => match round_trip::<LlmResponse>(&v) {
            Some(res) => Payload::LlmResponse(Box::new(res)),
            None => Payload::Json(v),
        },
        Payload::LlmRequest(_) => match llm_request_from_json(&v) {
            Some(req) => Payload::LlmRequest(Box::new(req)),
            None => Payload::Json(v),
        },
        Payload::Json(_) => Payload::Json(v),
    }
}

/// `v` as a `T` that serializes back to exactly `v`.
fn round_trip<T: Serialize + DeserializeOwned>(v: &Value) -> Option<T> {
    let t: T = serde_json::from_value(v.clone()).ok()?;
    (serde_json::to_value(&t).ok().as_ref() == Some(v)).then_some(t)
}

/// The `LlmRequest` of [`llm_request_json`]'s form, or `None` for any other value.
fn llm_request_from_json(v: &Value) -> Option<LlmRequest> {
    let m = v.as_object()?;
    if m.is_empty()
        || m.keys()
            .any(|k| !matches!(k.as_str(), "Model" | "Contents" | "Config"))
    {
        return None;
    }
    let model = match m.get("Model") {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(_) => return None,
    };
    let contents = match m.get("Contents") {
        None | Some(Value::Null) => Vec::new(),
        Some(c) => serde_json::from_value(c.clone()).ok()?,
    };
    let config = match m.get("Config") {
        None | Some(Value::Null) => None,
        Some(c) => Some(serde_json::from_value(c.clone()).ok()?),
    };
    Some(LlmRequest {
        model,
        contents,
        config,
        ..LlmRequest::default()
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use std::any::Any;
    use std::collections::BTreeMap;

    use chrono::{TimeZone, Utc};
    use gadk_agent::{AgentConfig, Context, Tool};
    use gadk_engine::llm::LlmAgentConfig;
    use gadk_engine::testutil::MockModel;
    use gadk_genai::{
        Content, FunctionDeclaration, Schema, ThinkingConfig, Tool as GenaiTool, Type,
    };
    use gadk_model::Llm;
    use gadk_session::{EventActions, NodeInfo, SessionError};
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::*;
    use crate::batch_processor::BatchProcessor;
    use crate::bigquery::fake::{FakeOffloader, RecordingLogger};
    use crate::config::{Config, ContentFormatter, Logger};
    use crate::formatter::smart_truncate_with;
    use crate::plugin::version::adk_version;
    use crate::plugin::{Payload, SCOPE_KIND_FUNCTION_CALL, SCOPE_KIND_NODE_RUN};
    use crate::redaction::{
        COLUMN_ATTRIBUTES, COLUMN_CONTENT, COLUMN_CONTENT_PARTS, COLUMN_LATENCY_MS,
        CustomMetadataFilter, parse_payload_column_denylist,
    };
    use crate::row::{Row, RowExt};
    use crate::trace::{Latency, RowSpan};

    /// Go's `&analyticsPlugin{config, processor, deniedColumns, customMetadata}` over a bare
    /// processor whose queue the test drains.
    fn plugin(config: Config) -> AnalyticsPlugin {
        AnalyticsPlugin {
            processor: Some(Arc::new(BatchProcessor::bare(config.clone(), 100))),
            denied_columns: parse_payload_column_denylist(&config.payload_column_denylist).unwrap(),
            custom_metadata: CustomMetadataFilter::new(&config.custom_metadata_allowlist),
            config,
            ..AnalyticsPlugin::default()
        }
    }

    fn quiet_config() -> Config {
        Config {
            logger: None,
            ..Config::default()
        }
    }

    fn json_of(row: &Row, column: &str) -> Value {
        serde_json::from_str(row.str(column).unwrap()).unwrap()
    }

    struct FakeTool {
        name: &'static str,
        description: &'static str,
        decl: Option<FunctionDeclaration>,
    }

    impl Tool for FakeTool {
        fn name(&self) -> &str {
            self.name
        }
        fn description(&self) -> &str {
            self.description
        }
        fn declaration(&self) -> Option<FunctionDeclaration> {
            self.decl.clone()
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    fn packed(t: FakeTool) -> Arc<dyn Any + Send + Sync> {
        let t: Arc<dyn Tool> = Arc::new(t);
        Arc::new(t)
    }

    struct MapState(Vec<(String, Value)>);

    impl ReadonlyState for MapState {
        fn get(&self, key: &str) -> Result<Value, SessionError> {
            self.0
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.clone())
                .ok_or(SessionError::StateKeyNotExist)
        }
        fn all(&self) -> Vec<(String, Value)> {
            self.0.clone()
        }
    }

    struct PanickingState;

    impl ReadonlyState for PanickingState {
        fn get(&self, _: &str) -> Result<Value, SessionError> {
            Err(SessionError::StateKeyNotExist)
        }
        fn all(&self) -> Vec<(String, Value)> {
            panic!("state unavailable")
        }
    }

    // Go TestParseNodePath.
    #[test]
    fn parse_node_path_table() {
        let cases: [(&str, &str, Value); 7] = [
            ("", "", Value::Null),
            ("agent", "", Value::Null),
            ("agent@1", "1", Value::Null),
            ("wf@0/agent@3", "3", json!("0")),
            ("wf/agent@3", "3", Value::Null),
            ("wf@0/agent", "", json!("0")),
            ("root/wf@0/agent@3", "3", json!("0")),
        ];
        for (path, run_id, parent) in cases {
            assert_eq!(parse_node_path(path), (run_id.to_owned(), parent), "{path}");
        }
        assert_eq!(segment_run_id("a@b@c"), Some("c".to_owned()));
        assert_eq!(segment_run_id("plain"), None);
    }

    // Go TestDeriveScope.
    #[test]
    fn derive_scope_table() {
        for (scope, kind) in [
            ("wf@0/review@2", SCOPE_KIND_NODE_RUN),
            ("review@2", SCOPE_KIND_NODE_RUN),
            ("adk-call-123", SCOPE_KIND_FUNCTION_CALL),
            ("wf@0/adk-call-123", SCOPE_KIND_FUNCTION_CALL),
        ] {
            assert_eq!(
                Value::Object(derive_scope(scope)),
                json!({"id": scope, "kind": kind}),
                "{scope}"
            );
        }
    }

    // Go TestCacheType.
    #[test]
    fn cache_type_table() {
        assert_eq!(cache_type(None), "none");
        let mut u = GenerateContentResponseUsageMetadata {
            prompt_token_count: 10,
            ..Default::default()
        };
        assert_eq!(cache_type(Some(&u)), "none");
        u.cached_content_token_count = 1;
        assert_eq!(cache_type(Some(&u)), "implicit");
    }

    #[test]
    fn new_event_id_is_32_lowercase_hex_digits() {
        let a = new_event_id();
        assert_eq!(a.len(), 32);
        assert!(
            a.chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
        );
        assert_ne!(a, new_event_id());
    }

    #[test]
    fn put_non_empty_skips_empty_values() {
        let mut m = Map::new();
        put_non_empty(&mut m, "a", "");
        put_non_empty(&mut m, "b", "x");
        assert_eq!(Value::Object(m), json!({"b": "x"}));
    }

    // Go TestIdentityOf_PlainContext and TestIdentityOf_BareInvocationContext.
    #[test]
    fn identity_of_plain_and_adk_contexts() {
        let id = identity_of(&CallbackCtx::background());
        assert_eq!(format!("{id:?}"), format!("{:?}", Identity::default()));
        assert!(runner_agent_of(&CallbackCtx::background()).is_none());

        let ctx = Context::builder()
            .invocation_id("inv-bare")
            .branch("root.bare")
            .build();
        let cb = CallbackCtx {
            adk: Some(ctx),
            ..CallbackCtx::default()
        };
        let id = identity_of(&cb);
        assert_eq!(id.agent_name, "test_agent");
        assert_eq!(id.session_id, "session");
        assert_eq!(id.invocation_id, "inv-bare");
        assert_eq!(id.user_id, "user");
        assert_eq!(id.app_name, "app");
        assert_eq!(id.branch, "root.bare");
        assert_eq!(id.path, "");
        assert!(id.state.is_some());
        assert_eq!(runner_agent_of(&cb).unwrap().name(), "test_agent");
    }

    /// Spec R8 / Go `contextPath`: the identity path is the context's node path, else the path
    /// the nearest fresh invocation forked from (a dynamic tool-run helper's own path is empty).
    #[test]
    fn identity_path_is_the_context_path() {
        let promoted = Context::builder()
            .build()
            .with_delta(gadk_agent::ContextDelta {
                path: Some("wf@1/task@1".into()),
                ..gadk_agent::ContextDelta::default()
            });
        let helper = promoted.with_delta(gadk_agent::ContextDelta {
            fresh_invocation: true,
            ..gadk_agent::ContextDelta::default()
        });
        assert_eq!(helper.path(), "");
        let cb = CallbackCtx {
            adk: Some(helper),
            ..CallbackCtx::default()
        };
        assert_eq!(identity_of(&cb).path, "wf@1/task@1");
    }

    #[test]
    fn session_metadata_with_and_without_state() {
        let mut id = Identity {
            session_id: "s".into(),
            app_name: "a".into(),
            user_id: "u".into(),
            ..Identity::default()
        };
        assert_eq!(
            Value::Object(session_metadata(&id)),
            json!({"session_id": "s", "app_name": "a", "user_id": "u"})
        );
        assert_eq!(state_snapshot(None), None);
        id.state = Some(Arc::new(MapState(vec![])));
        assert_eq!(state_snapshot(id.state.as_ref()), Some(Map::new()));
        assert!(!session_metadata(&id).contains_key("state"));
        id.state = Some(Arc::new(MapState(vec![("k".into(), json!(1))])));
        assert_eq!(session_metadata(&id)["state"], json!({"k": 1}));
        // Go `orZero`: a panicking state is left out.
        id.state = Some(Arc::new(PanickingState));
        assert!(!session_metadata(&id).contains_key("state"));
    }

    // Go TestLLMRequestAttributes_Tools.
    #[test]
    fn llm_request_attributes_tools() {
        let mut req = LlmRequest {
            config: Some(GenerateContentConfig {
                tools: vec![GenaiTool {
                    function_declarations: vec![FunctionDeclaration {
                        name: "from_config".into(),
                        description: "Declared in the config".into(),
                        parameters: Some(Schema {
                            r#type: Some(Type::Object),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
                ..Default::default()
            }),
            ..LlmRequest::default()
        };
        req.tools.insert(
            "lookup".into(),
            packed(FakeTool {
                name: "lookup",
                description: "Looks things up",
                decl: Some(FunctionDeclaration {
                    name: "lookup".into(),
                    description: "Declaration description".into(),
                    parameters: Some(Schema {
                        r#type: Some(Type::String),
                        ..Default::default()
                    }),
                    parameters_json_schema: Some(json!({"type": "object"})),
                    ..Default::default()
                }),
            }),
        );
        req.tools.insert(
            "from_config".into(),
            packed(FakeTool {
                name: "from_config",
                description: "",
                decl: None,
            }),
        );
        let attrs = llm_request_attributes(&req);
        let (b, _) = smart_truncate_with(&attrs["tools"], -1, true).unwrap();
        let got: Value = serde_json::from_slice(&b).unwrap();
        assert_eq!(
            got,
            json!([
                {"name": "from_config", "description": "Declared in the config", "parameters": {"type": "OBJECT"}},
                {"name": "lookup", "description": "Looks things up", "parameters": {"type": "object"}},
            ])
        );
        // An empty config adds nothing but the tools.
        assert!(!attrs.contains_key("llm_config"));
        assert!(!attrs.contains_key("model"));
    }

    #[test]
    fn tool_declarations_of_a_non_tool_entry_use_the_key() {
        let mut req = LlmRequest::default();
        assert!(tool_declarations(&req).is_empty());
        req.tools.insert("opaque".into(), Arc::new(7_u8));
        assert_eq!(
            tool_declarations(&req),
            vec![json!({"name": "opaque"}).as_object().unwrap().clone()]
        );
    }

    #[test]
    fn llm_config_and_labels() {
        assert_eq!(llm_config(None), None);
        assert_eq!(
            llm_config(Some(&GenerateContentConfig::default())),
            Some(Map::new())
        );
        let mut labels = BTreeMap::new();
        labels.insert("team".to_owned(), "a".to_owned());
        let req = LlmRequest {
            model: "gemini-test".into(),
            config: Some(GenerateContentConfig {
                temperature: Some(0.7),
                top_p: Some(0.95),
                top_k: Some(40.0),
                candidate_count: 1,
                max_output_tokens: 256,
                stop_sequences: vec!["END".into()],
                seed: Some(7),
                response_mime_type: "application/json".into(),
                response_logprobs: true,
                logprobs: Some(3),
                thinking_config: Some(ThinkingConfig {
                    include_thoughts: true,
                    thinking_budget: Some(128),
                    ..Default::default()
                }),
                labels,
                ..Default::default()
            }),
            ..LlmRequest::default()
        };
        let attrs = Value::Object(llm_request_attributes(&req));
        let text = String::from_utf8(gadk_model::to_go_json(&attrs).unwrap()).unwrap();
        // float32 settings keep Go's 32-bit text.
        assert!(text.contains(r#""temperature":0.7,"#), "{text}");
        assert!(text.contains(r#""top_p":0.95}"#), "{text}");
        assert_eq!(
            attrs,
            json!({
                "model": "gemini-test",
                "labels": {"team": "a"},
                "llm_config": {
                    "temperature": 0.7,
                    "top_p": 0.95,
                    "top_k": 40,
                    "candidate_count": 1,
                    "max_output_tokens": 256,
                    "stop_sequences": ["END"],
                    "response_mime_type": "application/json",
                    "seed": 7,
                    "response_logprobs": true,
                    "logprobs": 3,
                    "thinking_config": {"includeThoughts": true, "thinkingBudget": 128},
                },
            })
        );
    }

    #[test]
    fn llm_response_attributes_and_content() {
        let res = LlmResponse {
            content: Some(Content::from_text("hi", "model")),
            model_version: "v1".into(),
            partial: true,
            usage_metadata: Some(GenerateContentResponseUsageMetadata {
                prompt_token_count: 3,
                candidates_token_count: 4,
                total_token_count: 7,
                thoughts_token_count: 1,
                cached_content_token_count: 2,
                tool_use_prompt_token_count: 5,
                ..Default::default()
            }),
            finish_reason: Some("STOP".parse().unwrap()),
            ..LlmResponse::default()
        };
        assert_eq!(
            Value::Object(llm_response_attributes(&res)),
            json!({
                "partial": true,
                "turn_complete": false,
                "interrupted": false,
                "model_version": "v1",
                "usage_metadata": {
                    "prompt_token_count": 3,
                    "candidates_token_count": 4,
                    "total_token_count": 7,
                    "thoughts_token_count": 1,
                    "cached_content_token_count": 2,
                    "tool_use_prompt_token_count": 5,
                },
                "finish_reason": "STOP",
                "cache_type": "implicit",
            })
        );
        let (content, truncated, builder) = build_llm_response_content(&res, -1, true, false, None);
        assert_eq!(
            Value::Object(content.unwrap()),
            json!({
                "response": "text: 'hi'",
                "usage": {"prompt": 3, "completion": 4, "total": 7, "thoughts": 1, "cached": 2},
            })
        );
        assert!(!truncated);
        assert!(builder.is_some_and(|b| b.is_llm_response && !b.needs_rebuild));
        // Neither content nor usage: no content.
        let (content, truncated, builder) =
            build_llm_response_content(&LlmResponse::default(), -1, true, false, None);
        assert!(content.is_none() && !truncated && builder.is_none());
    }

    #[test]
    fn llm_request_content_prompt_and_system_prompt() {
        let req = LlmRequest {
            contents: vec![
                Content::from_text("hello", "user"),
                Content {
                    role: String::new(),
                    ..Content::from_text("answer", "model")
                },
            ],
            config: Some(GenerateContentConfig {
                system_instruction: Some(Content::from_text("be brief", "")),
                ..Default::default()
            }),
            ..LlmRequest::default()
        };
        let (content, truncated) = llm_request_content(&req, -1, true, None);
        assert!(!truncated);
        assert_eq!(
            Value::Object(content),
            json!({
                "prompt": [
                    {"role": "user", "content": "hello"},
                    {"role": "unknown", "content": "answer"},
                ],
                "system_prompt": "be brief",
            })
        );
        let (_, _, builder) = build_llm_request_content(&req, -1, true, true, None);
        let b = builder.unwrap();
        assert!(b.is_llm_request && b.content_denied);
        assert_eq!(b.messages.len(), 3);
        assert_eq!(b.messages[2].content_ord, 2);
    }

    #[test]
    fn agent_starting_data_of_custom_and_llm_agents() {
        let leaf = gadk_agent::new_agent(AgentConfig {
            name: "leaf".into(),
            ..Default::default()
        })
        .unwrap();
        let custom = gadk_agent::new_agent(AgentConfig {
            name: "custom".into(),
            description: "Runs things".into(),
            sub_agents: vec![leaf.clone()],
            ..Default::default()
        })
        .unwrap();
        let (content, attrs) = agent_starting_data(custom.as_ref());
        assert_eq!(content, None);
        assert_eq!(
            Value::Object(attrs),
            json!({"agent_description": "Runs things", "sub_agents": ["leaf"]})
        );

        let model: Arc<dyn Llm> = MockModel::new(vec![]);
        let llm = gadk_engine::llm::agent::new(LlmAgentConfig {
            name: "helper".into(),
            instruction: "Be helpful.".into(),
            model: Some(model.clone()),
            ..Default::default()
        })
        .unwrap();
        let (content, attrs) = agent_starting_data(llm.as_ref());
        assert_eq!(content, Some(json!("Be helpful.")));
        assert_eq!(Value::Object(attrs), json!({"model": "mock"}));

        let provided = gadk_engine::llm::agent::new(LlmAgentConfig {
            name: "provided".into(),
            instruction: "ignored".into(),
            instruction_provider: Some(Arc::new(|_| Box::pin(async { Ok(String::new()) }))),
            ..Default::default()
        })
        .unwrap();
        let (content, attrs) = agent_starting_data(provided.as_ref());
        assert_eq!(content, None);
        assert_eq!(
            Value::Object(attrs),
            json!({"instruction_source": "provider"})
        );
    }

    #[test]
    fn adk_envelope_with_source_event_and_extras() {
        let p = plugin(quiet_config());
        let mut delta = BTreeMap::new();
        delta.insert("report.txt".to_owned(), 2_i64);
        let ev = Event {
            id: "ev-1".into(),
            branch: "root.wf".into(),
            author: "writer".into(),
            isolation_scope: "wf@0/review@2".into(),
            routes: vec!["next".into()],
            node_info: Some(NodeInfo {
                path: "wf@0/agent@3".into(),
                message_as_output: true,
                output_for: vec!["wf@0".into()],
            }),
            actions: EventActions {
                artifact_delta: Some(delta),
                skip_summarization: true,
                escalate: true,
                ..Default::default()
            },
            ..Event::default()
        };
        let mut d = EventData {
            function_call_id: "call-1".into(),
            source_event: Some(Arc::new(ev)),
            ..EventData::default()
        };
        d.adk_extras.insert("pause_kind".into(), json!("HITL"));
        d.adk_extras.insert("event_id".into(), json!("not-this"));
        let id = Identity {
            app_name: "app".into(),
            invocation_id: "inv".into(),
            session_id: "sess".into(),
            user_id: "user".into(),
            ..Identity::default()
        };
        let adk = p.adk_envelope("evt", &id, "agent-a", &d);
        assert_eq!(
            Value::Object(adk),
            json!({
                "schema_version": "1",
                "event_id": "evt",
                "adk_version": adk_version(),
                "app_name": "app",
                "invocation_id": "inv",
                "function_call_id": "call-1",
                "session_id": "sess",
                "user_id": "user",
                "author": "agent-a",
                "source_event_id": "ev-1",
                "branch": "root.wf",
                "node": {
                    "path": "wf@0/agent@3",
                    "run_id": "3",
                    "parent_run_id": "0",
                    "message_as_output": true,
                    "output_for": ["wf@0"],
                },
                "isolation_scope": "wf@0/review@2",
                "scope": {"id": "wf@0/review@2", "kind": "node_run"},
                "route": ["next"],
                "artifact_delta": {"report.txt": 2},
                "skip_summarization": true,
                "escalate": true,
                "pause_kind": "HITL",
            })
        );
        // Without session metadata, the session and user IDs leave the envelope; an explicit
        // author wins over the agent name.
        let p = plugin(Config {
            disable_session_metadata: true,
            ..quiet_config()
        });
        let d = EventData {
            author: "user".into(),
            ..EventData::default()
        };
        let adk = p.adk_envelope("evt", &id, "agent-a", &d);
        assert!(!adk.contains_key("session_id") && !adk.contains_key("user_id"));
        assert_eq!(adk["author"], json!("user"));
    }

    #[test]
    fn build_row_writes_every_column() {
        let mut tags = Map::new();
        tags.insert("env".into(), json!("test"));
        let p = plugin(Config {
            custom_tags: tags,
            enable_otel_correlation: true,
            custom_metadata_allowlist: vec!["keep*".into()],
            ..quiet_config()
        });
        let span_ts = Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 5).unwrap();
        let ctx = CallbackCtx::background().with_span(SpanIds {
            trace_id: [0xab; 16],
            span_id: [0x01; 8],
        });
        let id = Identity {
            session_id: "sess".into(),
            invocation_id: "inv".into(),
            user_id: "user".into(),
            app_name: "app".into(),
            ..Identity::default()
        };
        let mut md = Map::new();
        md.insert("keep_me".into(), json!(1));
        md.insert("drop_me".into(), json!(2));
        let mut d = EventData {
            span: RowSpan {
                trace_id: "t1".into(),
                span_id: "s1".into(),
                parent_span_id: "p1".into(),
                latency: Some(Latency {
                    total_ms: 12,
                    time_to_first_token_ms: Some(5),
                }),
                root_agent_name: "root".into(),
                timestamp: Some(span_ts),
            },
            content: Some(Payload::Content(Content::from_text("hello", "user"))),
            source_event: Some(Arc::new(Event {
                author: "event-author".into(),
                ..Event::default()
            })),
            err_msg: "boom".into(),
            failed: true,
            custom_metadata: Some(md),
            ..EventData::default()
        };
        d.attributes.insert("model".into(), json!("m"));
        let q = p.build_row(&ctx, &id, "USER_MESSAGE_RECEIVED", &mut d);
        let row = &q.row;
        assert_eq!(row.timestamp("timestamp"), Some(span_ts));
        assert_eq!(row.str("event_id").map(str::len), Some(32));
        assert_eq!(row.str("event_type"), Some("USER_MESSAGE_RECEIVED"));
        assert_eq!(row.str("invocation_id"), Some("inv"));
        assert_eq!(row.str("user_id"), Some("user"));
        // An identity without an agent falls back to the source event's author.
        assert_eq!(row.str("agent"), Some("event-author"));
        assert_eq!(row.str("session_id"), Some("sess"));
        assert_eq!(row.str("trace_id"), Some("t1"));
        assert_eq!(row.str("span_id"), Some("s1"));
        assert_eq!(row.str("parent_span_id"), Some("p1"));
        assert_eq!(
            row.str("latency_ms"),
            Some(r#"{"time_to_first_token_ms":5,"total_ms":12}"#)
        );
        assert_eq!(row.str("status"), Some("ERROR"));
        assert_eq!(row.str("error_message"), Some("boom"));
        assert_eq!(row.bool("is_truncated"), Some(false));
        assert_eq!(row.str("content"), Some(r#"{"text_summary":"hello"}"#));
        assert_eq!(
            crate::row::to_json_value(&row["content_parts"]),
            json!([{"part_index": 0, "mime_type": "text/plain", "text": "hello", "storage_mode": "INLINE"}])
        );
        let attrs = json_of(row, "attributes");
        assert_eq!(attrs["model"], json!("m"));
        assert_eq!(attrs["root_agent_name"], json!("root"));
        assert_eq!(attrs["custom_tags"], json!({"env": "test"}));
        assert_eq!(
            attrs["session_metadata"],
            json!({"session_id": "sess", "app_name": "app", "user_id": "user"})
        );
        assert_eq!(
            attrs["otel"],
            json!({"span_id": "0101010101010101", "trace_id": "abababababababababababababababab"})
        );
        assert_eq!(attrs["custom_metadata"], json!({"keep_me": 1}));
        assert_eq!(attrs["adk"]["author"], json!("event-author"));
        assert_eq!(attrs["adk"]["event_id"].as_str(), row.str("event_id"));
        assert!(q.raw_parts.is_empty() && q.summary_builder.is_none());
    }

    #[test]
    fn build_row_timestamp_precedence_and_denied_columns() {
        let p = plugin(Config {
            payload_column_denylist: vec![
                COLUMN_ATTRIBUTES.into(),
                COLUMN_LATENCY_MS.into(),
                COLUMN_CONTENT.into(),
            ],
            ..quiet_config()
        });
        let own = Utc.with_ymd_and_hms(2026, 3, 1, 0, 0, 0).unwrap();
        let mut d = EventData {
            timestamp: Some(own),
            span: RowSpan {
                timestamp: Some(Utc.with_ymd_and_hms(2026, 4, 1, 0, 0, 0).unwrap()),
                latency: Some(Latency::default()),
                ..RowSpan::default()
            },
            content: Some(Payload::Json(json!({"x": 1}))),
            ..EventData::default()
        };
        let id = Identity {
            agent_name: "a".into(),
            ..Identity::default()
        };
        let row = p
            .build_row(&CallbackCtx::background(), &id, "AGENT_STARTING", &mut d)
            .row;
        assert_eq!(row.timestamp("timestamp"), Some(own));
        assert_eq!(row.str("agent"), Some("a"));
        for absent in [
            "attributes",
            "latency_ms",
            "content",
            "content_parts",
            "session_id",
            "trace_id",
            "error_message",
        ] {
            assert!(!row.contains_key(absent), "{absent}");
        }
        assert_eq!(row.str("status"), Some("OK"));
        // No timestamp at all: now.
        let before = Utc::now();
        let row = p
            .build_row(
                &CallbackCtx::background(),
                &id,
                "AGENT_STARTING",
                &mut EventData::default(),
            )
            .row;
        assert!(row.timestamp("timestamp").unwrap() >= before);
        // An invalid span (all zero) writes no otel section.
        let p = plugin(Config {
            enable_otel_correlation: true,
            ..quiet_config()
        });
        let ctx = CallbackCtx::background().with_span(SpanIds::default());
        let row = p
            .build_row(&ctx, &id, "AGENT_STARTING", &mut EventData::default())
            .row;
        assert!(json_of(&row, "attributes").get("otel").is_none());
    }

    #[test]
    fn build_row_redacts_and_truncates_the_error_message() {
        let p = plugin(Config {
            max_content_len: 8,
            ..quiet_config()
        });
        let mut d = EventData {
            err_msg: "a very long failure".into(),
            failed: true,
            ..EventData::default()
        };
        let row = p
            .build_row(
                &CallbackCtx::background(),
                &Identity::default(),
                "TOOL_ERROR",
                &mut d,
            )
            .row;
        assert_eq!(row.str("error_message"), Some("a very l...[TRUNCATED]"));
        assert_eq!(row.bool("is_truncated"), Some(true));
    }

    // Go TestAddContent_FormatterFailureWhenContentDeniedRecordsDropForContentParts.
    #[test]
    fn add_content_formatter_failure() {
        let failing: ContentFormatter = Arc::new(|_, _| Err("boom".into()));
        let p = plugin(Config {
            payload_column_denylist: vec![COLUMN_CONTENT.into()],
            content_formatter: Some(failing.clone()),
            ..quiet_config()
        });
        let mut row = QueuedRow::default();
        let mut d = EventData {
            content: Some(Payload::Content(Content::from_text("hello", "user"))),
            ..EventData::default()
        };
        assert!(!p.add_content(&mut row, "USER_MESSAGE_RECEIVED", &mut d));
        assert_eq!(d.attributes["formatter_failed"], json!(true));
        assert!(row.row.is_empty());
        let stats = p.processor.as_ref().unwrap().drop_stats();
        assert_eq!((stats.formatter_failure_drops, stats.total_drops), (1, 0));

        // Both content columns denied: the formatter does not run.
        let p = plugin(Config {
            payload_column_denylist: vec![COLUMN_CONTENT.into(), COLUMN_CONTENT_PARTS.into()],
            content_formatter: Some(failing.clone()),
            ..quiet_config()
        });
        let mut d = EventData {
            content: Some(Payload::Content(Content::from_text("hello", "user"))),
            ..EventData::default()
        };
        assert!(!p.add_content(&mut QueuedRow::default(), "USER_MESSAGE_RECEIVED", &mut d));
        assert_eq!(
            p.processor
                .as_ref()
                .unwrap()
                .drop_stats()
                .formatter_failure_drops,
            0
        );

        // Content written: the placeholder, and the note joins an existing error message.
        let logger = Arc::new(RecordingLogger::default());
        let p = plugin(Config {
            content_formatter: Some(failing),
            logger: Some(logger.clone() as Arc<dyn Logger>),
            ..Config::default()
        });
        let mut row = QueuedRow::default();
        let mut d = EventData {
            content: Some(Payload::LlmRequest(Box::new(LlmRequest {
                model: "gemini-2.5-flash".into(),
                ..LlmRequest::default()
            }))),
            err_msg: "model failed".into(),
            ..EventData::default()
        };
        assert!(!p.add_content(&mut row, "LLM_REQUEST", &mut d));
        assert_eq!(row.row.str("content"), Some(r#""[FORMATTER_FAILED]""#));
        assert_eq!(d.err_msg, "model failed; content_formatter raised error");
        assert!(logger.contains("ContentFormatter failed for event LLM_REQUEST"));
    }

    #[test]
    fn add_content_formatter_results() {
        // An identity formatter keeps the typed path (summary and content_parts).
        let identity: ContentFormatter = Arc::new(|v, _| Ok(v.clone()));
        let p = plugin(Config {
            content_formatter: Some(identity),
            ..quiet_config()
        });
        let mut row = QueuedRow::default();
        let mut d = EventData {
            content: Some(Payload::Content(Content::from_text("hello", "user"))),
            ..EventData::default()
        };
        assert!(!p.add_content(&mut row, "USER_MESSAGE_RECEIVED", &mut d));
        assert_eq!(row.row.str("content"), Some(r#"{"text_summary":"hello"}"#));
        assert!(row.row.contains_key("content_parts"));

        // A formatter that rewrites the text of a typed payload keeps its type.
        let rewrite: ContentFormatter = Arc::new(|v, _| {
            Ok(serde_json::from_str(&v.to_string().replace("secret", "[HIDDEN]")).unwrap())
        });
        let p = plugin(Config {
            content_formatter: Some(rewrite),
            ..quiet_config()
        });
        let mut row = QueuedRow::default();
        let mut d = EventData {
            content: Some(Payload::LlmRequest(Box::new(LlmRequest {
                contents: vec![Content::from_text("my secret", "user")],
                ..LlmRequest::default()
            }))),
            ..EventData::default()
        };
        p.add_content(&mut row, "LLM_REQUEST", &mut d);
        assert_eq!(
            row.row.str("content"),
            Some(r#"{"prompt":[{"content":"my [HIDDEN]","role":"user"}]}"#)
        );
        assert_eq!(
            crate::row::to_json_value(&row.row["content_parts"])[0]["text"],
            json!("my [HIDDEN]")
        );

        // A formatter returning another shape writes it as JSON, without content_parts.
        let redact: ContentFormatter = Arc::new(|_, _| Ok(json!({"response": "[REDACTED]"})));
        let p = plugin(Config {
            content_formatter: Some(redact),
            ..quiet_config()
        });
        let mut row = QueuedRow::default();
        let mut d = EventData {
            content: Some(Payload::Content(Content::from_text("hello", "user"))),
            parts: vec![json!({"part_index": 0}).as_object().unwrap().clone()],
            ..EventData::default()
        };
        p.add_content(&mut row, "AGENT_RESPONSE", &mut d);
        assert_eq!(row.row.str("content"), Some(r#"{"response":"[REDACTED]"}"#));
        assert!(!row.row.contains_key("content_parts"));

        // A formatter returning null drops the content.
        let null: ContentFormatter = Arc::new(|_, _| Ok(Value::Null));
        let p = plugin(Config {
            content_formatter: Some(null),
            ..quiet_config()
        });
        let mut row = QueuedRow::default();
        let mut d = EventData {
            content: Some(Payload::Content(Content::from_text("hello", "user"))),
            ..EventData::default()
        };
        assert!(!p.add_content(&mut row, "USER_MESSAGE_RECEIVED", &mut d));
        assert!(row.row.is_empty());
    }

    #[test]
    fn add_content_json_payload_with_preformatted_parts() {
        let p = plugin(quiet_config());
        let mut row = QueuedRow::default();
        let mut d = EventData {
            content: Some(Payload::Json(json!({"response": "done"}))),
            parts: vec![
                json!({"part_index": 0, "text": "done"})
                    .as_object()
                    .unwrap()
                    .clone(),
            ],
            parts_truncated: true,
            ..EventData::default()
        };
        assert!(p.add_content(&mut row, "AGENT_RESPONSE", &mut d));
        assert_eq!(row.row.str("content"), Some(r#"{"response":"done"}"#));
        assert_eq!(
            crate::row::to_json_value(&row.row["content_parts"]),
            json!([{"part_index": 0, "text": "done"}])
        );
        // Multi-modal content off: no content_parts.
        let p = plugin(Config {
            disable_multi_modal_content: true,
            ..quiet_config()
        });
        let mut row = QueuedRow::default();
        assert!(!p.add_content(&mut row, "AGENT_RESPONSE", &mut d));
        assert!(!row.row.contains_key("content_parts"));
        // A JSON null is Go's nil content.
        let mut d = EventData {
            content: Some(Payload::Json(Value::Null)),
            ..EventData::default()
        };
        assert!(!p.add_content(&mut row, "AGENT_RESPONSE", &mut d));
    }

    #[test]
    fn add_content_offloads_long_text_through_the_queued_row() {
        let p = plugin(Config {
            gcs_offloader: Some(Arc::new(FakeOffloader::with_bucket("bucket"))),
            max_inline_text_size: 20,
            ..quiet_config()
        });
        let text = "x".repeat(60);
        let mut row = QueuedRow::default();
        let mut d = EventData {
            content: Some(Payload::LlmRequest(Box::new(LlmRequest {
                contents: vec![Content::from_text(text.as_str(), "user")],
                ..LlmRequest::default()
            }))),
            ..EventData::default()
        };
        p.add_content(&mut row, "LLM_REQUEST", &mut d);
        assert_eq!(row.raw_parts.len(), 1);
        assert!(row.raw_parts[0].is_text && row.raw_parts[0].reserved > 0);
        // The offloaded text leaves the summary until its upload resolves.
        assert_eq!(
            row.row.str("content"),
            Some(r#"{"prompt":[{"content":"","role":"user"}]}"#)
        );
        let builder = row.summary_builder.as_ref().unwrap().lock().unwrap();
        assert!(builder.is_llm_request && !builder.needs_rebuild);
        drop(builder);
        // Dropping the row's uploads releases the reservation.
        let processor = p.processor.as_ref().unwrap();
        processor.release_row_uploads(&mut row);
        assert!(
            processor.try_reserve_upload_bytes(crate::config::DEFAULT_MAX_PENDING_UPLOAD_BYTES)
        );
    }

    /// R4: the prompt attribute is the recorded prompt of the callback's agent, `label` omitted
    /// when unset; none without an ADK context or a record.
    #[test]
    fn prompt_attribute_is_the_recorded_prompt() {
        assert_eq!(prompt_attribute(&CallbackCtx::background()), None);
        let ctx = Context::builder().build();
        let cb = CallbackCtx::from_context(&ctx);
        assert_eq!(prompt_attribute(&cb), None);
        ctx.record_prompt(gadk_agent::PromptUse::new("triage", 3).with_label("production"));
        assert_eq!(
            prompt_attribute(&cb),
            Some(json!({"name": "triage", "version": 3, "label": "production"}))
        );
        ctx.record_prompt(gadk_agent::PromptUse::new("triage", 4));
        assert_eq!(
            prompt_attribute(&cb),
            Some(json!({"name": "triage", "version": 4}))
        );
    }
}
