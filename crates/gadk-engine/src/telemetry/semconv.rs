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

//! Port of Go `internal/telemetry/experimental_semconv.go`: the default (1.44) schema's request
//! content on the `generate_content` span and its `gen_ai.client.inference.operation.details`
//! log record.

use gadk_model::LlmRequest;
use serde_json::Value;

use super::attrs::{
    ERROR_TYPE, GCP_VERTEX_AGENT_INVOCATION_ID, GEN_AI_OPERATION_NAME, GEN_AI_REQUEST_MODEL,
    GenerateContentParams, GenerateContentResult, generate_content_result_attributes, record_attrs,
};
use super::converters::{AttrValue, from_log_value};
use super::env::{capture_content_on_spans, capture_message_content, use_legacy_schema};
use super::genai_messages::{
    ContentAttr, event_content_attributes, request_content, response_content,
    span_content_attributes,
};
use super::inference::{GEN_AI_PROVIDER_NAME, provider_name};

/// Go `inferenceOperationDetailsEventName`.
pub(crate) const INFERENCE_OPERATION_DETAILS_EVENT_NAME: &str =
    "gen_ai.client.inference.operation.details";

/// Go `operationDetails`: what the inference-details record says about the request.
pub(crate) struct OperationDetails {
    span: tracing::Span,
    /// Go `request`: the request content last noted.
    request: Vec<ContentAttr>,
    /// Go `eventRequest`: that content as the record's attributes.
    pub(crate) event_request: Vec<(&'static str, AttrValue)>,
}

impl OperationDetails {
    /// Details recorded on `span`.
    pub(crate) fn new(span: tracing::Span) -> Self {
        Self {
            span,
            request: Vec::new(),
            event_request: Vec::new(),
        }
    }

    /// Go `noteLLMRequest`: converts `req` (only when content goes to the span or the record),
    /// and on a change records it on the span and keeps it for the record. Span attributes cannot
    /// be removed, so an edit that drops a message key leaves the earlier value.
    pub(crate) fn note_llm_request(&mut self, req: &LlmRequest) {
        if use_legacy_schema() {
            return;
        }
        let on_span = !self.span.is_disabled() && capture_content_on_spans();
        let on_event = capture_message_content();
        if !on_span && !on_event {
            return;
        }
        let content = request_content(req);
        if content == self.request {
            return;
        }
        self.request = content;
        if on_span {
            record_attrs(&self.span, &span_content_attributes(&self.request));
        }
        if on_event {
            self.event_request = event_content_attributes(&self.request);
        }
    }

    /// Go `emit`: the `gen_ai.client.inference.operation.details` record (DEBUG, no body) under
    /// the span; default schema only.
    pub(crate) fn emit(&self, params: &GenerateContentParams, result: &GenerateContentResult) {
        if use_legacy_schema() {
            return;
        }
        let mut attrs: Vec<(&'static str, AttrValue)> = vec![
            (GEN_AI_OPERATION_NAME, AttrValue::string("generate_content")),
            (
                GEN_AI_REQUEST_MODEL,
                AttrValue::string(params.model_name.as_str()),
            ),
            (
                GCP_VERTEX_AGENT_INVOCATION_ID,
                AttrValue::string(params.invocation_id.as_str()),
            ),
        ];
        if let Some(provider) = provider_name(&params.model_name, params.backend) {
            attrs.push((GEN_AI_PROVIDER_NAME, AttrValue::String(provider)));
        }
        if let Some(e) = &result.error {
            attrs.push((ERROR_TYPE, AttrValue::string(e.error_type.as_str())));
        }
        attrs.extend(generate_content_result_attributes(result));
        if capture_message_content() {
            attrs.extend(self.event_request.iter().cloned());
            attrs.extend(event_content_attributes(&response_content(result)));
        }
        let attrs: Vec<(String, Value)> = attrs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), from_log_value(v)))
            .collect();
        // The spec makes this event opt-in; DEBUG lets consumers filter it out.
        super::emit_log_record(
            &self.span,
            tracing::Level::DEBUG,
            INFERENCE_OPERATION_DETAILS_EVENT_NAME,
            &Value::Null,
            &attrs,
        );
    }
}
