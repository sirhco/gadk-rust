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

//! Port of the span attributes of Go `internal/telemetry/telemetry.go`: the start and result
//! attributes of the `generate_content` and `execute_tool` spans, the merged tool-call span,
//! `RecordError` and `errorType`, honoring the content-capture mode and the legacy (1.36)
//! schema switch of [`env`](super::env).

use std::sync::Arc;

use gadk_model::{Backend, LlmRequest, LlmResponse};
use gadk_session::Event;
use serde::Serialize;
use serde_json::{Map, Value};

use super::converters::AttrValue;
use super::env::{capture_content_on_spans, use_legacy_schema};
use super::genai_messages::{
    ContentAttr, ContentValue, reported_finish_reason, request_content, response_content,
    schema_finish_reason, span_content_attributes, tool_payload,
};
use super::inference::{GEN_AI_PROVIDER_NAME, provider_name};

/// Go `executeToolName`.
pub(crate) const EXECUTE_TOOL_NAME: &str = "execute_tool";
/// Go `mergeToolName`.
pub(crate) const MERGE_TOOL_NAME: &str = "(merged tools)";

/// Go `gcpVertexAgentToolCallArgsName`.
pub(crate) const GCP_VERTEX_AGENT_TOOL_CALL_ARGS: &str = "gcp.vertex.agent.tool_call_args";
/// Go `gcpVertexAgentEventID`.
pub(crate) const GCP_VERTEX_AGENT_EVENT_ID: &str = "gcp.vertex.agent.event_id";
/// Go `gcpVertexAgentToolResponseName`.
pub(crate) const GCP_VERTEX_AGENT_TOOL_RESPONSE: &str = "gcp.vertex.agent.tool_response";
/// Go `gcpVertexAgentInvocationID`.
pub(crate) const GCP_VERTEX_AGENT_INVOCATION_ID: &str = "gcp.vertex.agent.invocation_id";
/// Go `genAIUsageCacheReadInputTokens`.
pub(crate) const GEN_AI_USAGE_CACHE_READ_INPUT_TOKENS: &str =
    "gen_ai.usage.cache_read.input_tokens";
/// Go `genAIUsageReasoningOutputTokens`.
pub(crate) const GEN_AI_USAGE_REASONING_OUTPUT_TOKENS: &str =
    "gen_ai.usage.reasoning.output_tokens";
/// Go `genAIToolCallArguments`.
pub(crate) const GEN_AI_TOOL_CALL_ARGUMENTS: &str = "gen_ai.tool.call.arguments";
/// Go `genAIToolCallResult`.
pub(crate) const GEN_AI_TOOL_CALL_RESULT: &str = "gen_ai.tool.call.result";
/// semconv `gen_ai.operation.name`.
pub(crate) const GEN_AI_OPERATION_NAME: &str = "gen_ai.operation.name";
/// semconv `gen_ai.request.model`.
pub(crate) const GEN_AI_REQUEST_MODEL: &str = "gen_ai.request.model";
/// semconv `gen_ai.response.finish_reasons`.
pub(crate) const GEN_AI_RESPONSE_FINISH_REASONS: &str = "gen_ai.response.finish_reasons";
/// semconv `gen_ai.usage.input_tokens`.
pub(crate) const GEN_AI_USAGE_INPUT_TOKENS: &str = "gen_ai.usage.input_tokens";
/// semconv `gen_ai.usage.output_tokens`.
pub(crate) const GEN_AI_USAGE_OUTPUT_TOKENS: &str = "gen_ai.usage.output_tokens";
/// semconv `gen_ai.tool.name`.
pub(crate) const GEN_AI_TOOL_NAME: &str = "gen_ai.tool.name";
/// semconv `gen_ai.tool.description`.
pub(crate) const GEN_AI_TOOL_DESCRIPTION: &str = "gen_ai.tool.description";
/// semconv `gen_ai.tool.call.id`.
pub(crate) const GEN_AI_TOOL_CALL_ID: &str = "gen_ai.tool.call.id";
/// semconv `error.type`.
pub(crate) const ERROR_TYPE: &str = "error.type";

/// semconv v1.36.0 `SchemaURL`.
pub(crate) const SCHEMA_URL_1_36: &str = "https://opentelemetry.io/schemas/1.36.0";
/// The default (1.44) schema URL.
pub(crate) const SCHEMA_URL_1_44: &str = "https://opentelemetry.io/schemas/1.44.0";

/// Go `errorType` and its value for an error that is not a `genai.APIError` (shared with
/// gadk-agent's agent spans).
pub(crate) use gadk_agent::agent::telemetry::{GO_ERROR_STRING_TYPE, error_type};

/// Go `schemaURL`: the schema of the tracer and the logger, following the env. `tracing` has no
/// instrumentation scope, so nothing records it; the debug view does not show it.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn schema_url() -> &'static str {
    if use_legacy_schema() {
        SCHEMA_URL_1_36
    } else {
        SCHEMA_URL_1_44
    }
}

/// An error as a span records it: Go's `err.Error()` (the status description) and
/// [`error_type`] (`error.type`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SpanError {
    /// Go `err.Error()`.
    pub(crate) message: String,
    /// Go `errorType(err)`.
    pub(crate) error_type: String,
}

impl SpanError {
    /// The span form of `err`.
    pub(crate) fn new(err: &(dyn std::error::Error + 'static)) -> Self {
        Self {
            message: err.to_string(),
            error_type: error_type(err),
        }
    }

    /// The span form of a Go `errors.New(message)`.
    pub(crate) fn text(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            error_type: GO_ERROR_STRING_TYPE.to_owned(),
        }
    }
}

/// Go `RecordError`: marks `span` failed with `err` (status `ERROR`, the description in
/// `error.message`, and an error event standing in for OTel's `exception` event) and, in the
/// default schema, sets `error.type`. `None` records nothing.
pub(crate) fn record_error(span: &tracing::Span, err: Option<&SpanError>) {
    let Some(err) = err else { return };
    span.record("otel.status_code", "ERROR");
    span.record("error.message", err.message.as_str());
    let name = span.metadata().map_or("span", |m| m.name());
    tracing::error!(
        parent: span,
        otel.status_code = "ERROR",
        otel.status_description = err.message.as_str(),
        "{name} failed"
    );
    if !use_legacy_schema() {
        span.record(ERROR_TYPE, err.error_type.as_str());
    }
}

/// Records each `(key, value)` on `span`.
pub(crate) fn record_attrs(span: &tracing::Span, attrs: &[(&'static str, AttrValue)]) {
    for (k, v) in attrs {
        v.record(span, k);
    }
}

/// Go `GenerateContentParams` (the request is passed separately).
#[derive(Debug, Clone, Default)]
pub(crate) struct GenerateContentParams {
    /// Go `ModelName`.
    pub(crate) model_name: String,
    /// Go `InvocationID`.
    pub(crate) invocation_id: String,
    /// Go `Backend` (`None` = `BackendUnspecified`).
    pub(crate) backend: Option<Backend>,
    /// Go `Stream`: whether the model was asked to stream.
    pub(crate) stream: bool,
}

/// Go `startGenerateContentSpan`: the invocation ID (read by adk-web), operation, model and, in
/// the default schema, the provider; the legacy schema with span content capture also records
/// the request content (the default schema records it through
/// [`OperationDetails`](super::semconv::OperationDetails)).
pub(crate) fn start_generate_content_span(
    params: &GenerateContentParams,
    req: &LlmRequest,
) -> tracing::Span {
    let span = super::generate_content_span(&params.model_name, &params.invocation_id);
    let legacy = use_legacy_schema();
    if !legacy && let Some(provider) = provider_name(&params.model_name, params.backend) {
        span.record(GEN_AI_PROVIDER_NAME, provider.as_str());
    }
    if legacy && !span.is_disabled() && capture_content_on_spans() {
        record_attrs(&span, &span_content_attributes(&request_content(req)));
    }
    span
}

/// Go `generateContentResult`: what a `generate_content` call produced.
#[derive(Debug, Clone, Default)]
pub(crate) struct GenerateContentResult {
    /// Go `Response`: the latest response of the call.
    pub(crate) response: Option<Arc<LlmResponse>>,
    /// Go `EventID`: the event ID reserved for [`Self::response`].
    pub(crate) event_id: String,
    /// Go `Error`.
    pub(crate) error: Option<SpanError>,
    /// Go `Responses`: every response of the call in order, ending with [`Self::response`];
    /// empty means [`Self::response`] alone. The legacy schema reads only the latter.
    pub(crate) responses: Vec<Arc<LlmResponse>>,
}

impl GenerateContentResult {
    /// Go `responses()`.
    pub(crate) fn responses(&self) -> Vec<Arc<LlmResponse>> {
        if self.responses.is_empty()
            && let Some(r) = &self.response
        {
            return vec![r.clone()];
        }
        self.responses.clone()
    }

    /// Go `usage()`: the latest reported usage (the legacy schema reads only the latest
    /// response).
    fn usage(&self) -> Option<gadk_genai::GenerateContentResponseUsageMetadata> {
        if use_legacy_schema() {
            return self.response.as_ref()?.usage_metadata.clone();
        }
        self.responses()
            .iter()
            .rev()
            .find_map(|r| r.usage_metadata.clone())
    }
}

/// Go `traceGenerateContentResult`.
pub(crate) fn trace_generate_content_result(span: &tracing::Span, r: &GenerateContentResult) {
    record_error(span, r.error.as_ref());
    record_attrs(span, &generate_content_result_attributes(r));
    if capture_content_on_spans() {
        record_attrs(span, &span_content_attributes(&response_content(r)));
    }
}

/// Go `generateContentResultAttributes`: the content-free result attributes.
pub(crate) fn generate_content_result_attributes(
    r: &GenerateContentResult,
) -> Vec<(&'static str, AttrValue)> {
    let mut attrs = Vec::new();
    let resp = r.response.as_deref();
    if resp.is_some() || r.error.is_some() {
        let finish_reason = if use_legacy_schema() {
            schema_finish_reason(resp, r.error.is_some())
        } else {
            // Omitted rather than guessed when the model reported none.
            reported_finish_reason(resp, r.error.is_some())
        };
        if !finish_reason.is_empty() {
            attrs.push((
                GEN_AI_RESPONSE_FINISH_REASONS,
                AttrValue::StringSlice(vec![finish_reason.into_owned()]),
            ));
        }
    }
    if resp.is_none() {
        return attrs;
    }
    attrs.push((
        GCP_VERTEX_AGENT_EVENT_ID,
        AttrValue::string(r.event_id.as_str()),
    ));
    if let Some(u) = r.usage() {
        attrs.extend([
            (
                GEN_AI_USAGE_INPUT_TOKENS,
                AttrValue::Int64(
                    i64::from(u.prompt_token_count) + i64::from(u.tool_use_prompt_token_count),
                ),
            ),
            (
                GEN_AI_USAGE_OUTPUT_TOKENS,
                AttrValue::Int64(
                    i64::from(u.candidates_token_count) + i64::from(u.thoughts_token_count),
                ),
            ),
            (
                GEN_AI_USAGE_CACHE_READ_INPUT_TOKENS,
                AttrValue::Int64(i64::from(u.cached_content_token_count)),
            ),
            (
                GEN_AI_USAGE_REASONING_OUTPUT_TOKENS,
                AttrValue::Int64(i64::from(u.thoughts_token_count)),
            ),
        ]);
    }
    attrs
}

/// Go `StartExecuteToolSpan`: the legacy schema records the arguments as JSON; the default one
/// records them only with span content capture (and not for Go's nil arguments, an empty map
/// here).
pub(crate) fn start_execute_tool_span(tool_name: &str, args: &Map<String, Value>) -> tracing::Span {
    let span = super::execute_tool_span(tool_name);
    if use_legacy_schema() {
        span.record(
            GCP_VERTEX_AGENT_TOOL_CALL_ARGS,
            safe_serialize(&tool_payload(args)).as_str(),
        );
    } else if !args.is_empty() && !span.is_disabled() && capture_content_on_spans() {
        record_attrs(
            &span,
            &span_content_attributes(&[ContentAttr {
                key: GEN_AI_TOOL_CALL_ARGUMENTS,
                value: ContentValue::Raw(tool_payload(args)),
            }]),
        );
    }
    span
}

/// Go `TraceToolResult`: the tool's description and response event; the default schema adds
/// the call ID and (with span content capture, on success) the response; the legacy one the call
/// ID and response JSON, `<not specified>` when unknown.
pub(crate) fn trace_tool_result(
    span: &tracing::Span,
    description: &str,
    response_event: Option<&Event>,
    err: Option<&SpanError>,
) {
    record_error(span, err);
    let mut attrs = vec![
        (GEN_AI_OPERATION_NAME, AttrValue::string(EXECUTE_TOOL_NAME)),
        (GEN_AI_TOOL_DESCRIPTION, AttrValue::string(description)),
    ];
    let mut function_response = None;
    if let Some(ev) = response_event {
        attrs.push((GCP_VERTEX_AGENT_EVENT_ID, AttrValue::string(ev.id.as_str())));
        function_response = ev
            .response
            .content
            .as_ref()
            .and_then(|c| c.parts.first())
            .and_then(|p| p.function_response.as_ref());
    }
    if !use_legacy_schema() {
        if let Some(fr) = function_response {
            if !fr.id.is_empty() {
                attrs.push((GEN_AI_TOOL_CALL_ID, AttrValue::string(fr.id.as_str())));
            }
            if capture_content_on_spans() && !fr.response.is_empty() && err.is_none() {
                attrs.extend(span_content_attributes(&[ContentAttr {
                    key: GEN_AI_TOOL_CALL_RESULT,
                    value: ContentValue::Raw(tool_payload(&fr.response)),
                }]));
            }
        }
        record_attrs(span, &attrs);
        return;
    }
    let mut call_id = "<not specified>".to_owned();
    let mut response = "<not specified>".to_owned();
    if let Some(fr) = function_response {
        if !fr.id.is_empty() {
            call_id = fr.id.clone();
        }
        if !fr.response.is_empty() {
            response = safe_serialize(&fr.response);
        }
    }
    attrs.push((GEN_AI_TOOL_CALL_ID, AttrValue::String(call_id)));
    attrs.push((GCP_VERTEX_AGENT_TOOL_RESPONSE, AttrValue::String(response)));
    record_attrs(span, &attrs);
}

/// Go `TraceMergedToolCallsResult`: the merged span's result (`fn_response_event` is the merged
/// response event).
pub(crate) fn trace_merged_tool_calls_result(
    span: &tracing::Span,
    fn_response_event: Option<&Event>,
    err: Option<&SpanError>,
) {
    record_error(span, err);
    let mut attrs = vec![
        (GEN_AI_OPERATION_NAME, AttrValue::string(EXECUTE_TOOL_NAME)),
        (GEN_AI_TOOL_NAME, AttrValue::string(MERGE_TOOL_NAME)),
        (GEN_AI_TOOL_DESCRIPTION, AttrValue::string(MERGE_TOOL_NAME)),
    ];
    if use_legacy_schema() {
        attrs.push((GCP_VERTEX_AGENT_TOOL_CALL_ARGS, AttrValue::string("N/A")));
        attrs.push((
            GCP_VERTEX_AGENT_TOOL_RESPONSE,
            AttrValue::String(safe_serialize(&fn_response_event)),
        ));
    }
    if let Some(ev) = fn_response_event {
        attrs.push((GCP_VERTEX_AGENT_EVENT_ID, AttrValue::string(ev.id.as_str())));
    }
    record_attrs(span, &attrs);
}

/// Go `safeSerialize`: Go's `json.Marshal` text, `<not serializable>` on failure.
pub(crate) fn safe_serialize<T: Serialize + ?Sized>(v: &T) -> String {
    gadk_model::to_go_json(v)
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
        .unwrap_or_else(|| "<not serializable>".to_owned())
}
