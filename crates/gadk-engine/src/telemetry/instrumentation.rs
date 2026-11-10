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

//! Port of Go `internal/telemetry/instrumentation.go`: `InstrumentInvocation` (the runner's
//! `invoke_workflow` span) and `InstrumentGenerateContent` (the `generate_content` span over a
//! model call).

use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context as TaskContext, Poll};

use futures::Stream;
use futures::stream::BoxStream;
use gadk_agent::{Agent, AgentError, AgentType, EventStream};
use gadk_model::{LlmRequest, LlmResponse, ModelError};
use gadk_session::Event;

use super::attrs::{
    GenerateContentParams, GenerateContentResult, SpanError, record_error,
    start_generate_content_span, trace_generate_content_result,
};
use super::env::use_legacy_schema;
use super::genai_messages::reported_finish_reason;
use super::semconv::OperationDetails;
use super::{in_workflow_scope, workflow_span};

/// Go `InstrumentInvocation`: runs `run` inside an `invoke_workflow` span named after `root`
/// (conversation `session_id`) whose status is the first error the stream yields. No span in
/// the legacy schema, for a workflow-agent root (it opens its own), or inside another
/// `invoke_workflow` span (a nested invocation, e.g. an agent tool). The span is entered on
/// every poll and ends with the stream or when the stream is dropped.
pub(crate) fn instrument_invocation(
    root: &dyn Agent,
    session_id: &str,
    run: impl FnOnce() -> EventStream,
) -> EventStream {
    if use_legacy_schema()
        || root.agent_state().agent_type == AgentType::WorkflowAgent
        || in_workflow_scope()
    {
        return run();
    }
    let span = workflow_span(root.name(), session_id);
    let inner = span.in_scope(run);
    Box::pin(InvocationStream {
        inner,
        span: Some(span),
        first_error: None,
    })
}

struct InvocationStream {
    inner: EventStream,
    span: Option<tracing::Span>,
    first_error: Option<SpanError>,
}

impl InvocationStream {
    fn end(&mut self) {
        if let Some(span) = self.span.take() {
            record_error(&span, self.first_error.as_ref());
        }
    }
}

impl Stream for InvocationStream {
    type Item = Result<Event, AgentError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        let poll = match &this.span {
            Some(span) => {
                let _entered = span.enter();
                this.inner.as_mut().poll_next(cx)
            }
            None => this.inner.as_mut().poll_next(cx),
        };
        match &poll {
            Poll::Ready(Some(Err(e))) if this.first_error.is_none() => {
                this.first_error = Some(SpanError::new(e));
            }
            Poll::Ready(None) => this.end(),
            _ => {}
        }
        poll
    }
}

impl Drop for InvocationStream {
    fn drop(&mut self) {
        self.end();
    }
}

/// One item of a model call: the response or error, and the event ID reserved for it (Go's
/// `responseWithEventID`).
pub(crate) type ModelItem = (Result<LlmResponse, ModelError>, String);

/// Go `InstrumentGenerateContent`: runs `call` inside a `generate_content` span. The span ends
/// at the first error, the first response that reports a finish reason, the first response
/// when the call does not stream, or the end of the stream (the legacy schema: the first error
/// or non-partial response), whichever is first, or when the stream is dropped; at its end it
/// gets the result attributes and, in the default schema, the inference-details log record.
/// `call` receives `req` once the span has read it.
pub(crate) fn instrument_generate_content(
    params: GenerateContentParams,
    req: LlmRequest,
    call: impl FnOnce(LlmRequest) -> BoxStream<'static, ModelItem>,
) -> BoxStream<'static, ModelItem> {
    let span = start_generate_content_span(&params, &req);
    // Go logRequest (the legacy schema's request log records) is emitted here (Task 11).
    let mut details = OperationDetails::new(span.clone());
    details.note_llm_request(&req);
    let inner = span.in_scope(|| call(req));
    Box::pin(GenerateContentStream {
        inner,
        legacy: use_legacy_schema(),
        state: Some(GenerateState {
            span,
            details,
            params,
            result: GenerateContentResult::default(),
        }),
    })
}

struct GenerateState {
    span: tracing::Span,
    details: OperationDetails,
    params: GenerateContentParams,
    result: GenerateContentResult,
}

impl GenerateState {
    /// Go's `end`: Rust passes the request to the model by value, so the model cannot edit it
    /// and Go's second `noteLLMRequest` has nothing to note.
    fn end(self) {
        trace_generate_content_result(&self.span, &self.result);
        self.details.emit(&self.params, &self.result);
    }
}

struct GenerateContentStream {
    inner: BoxStream<'static, ModelItem>,
    legacy: bool,
    state: Option<GenerateState>,
}

impl GenerateContentStream {
    fn end(&mut self) {
        if let Some(st) = self.state.take() {
            st.end();
        }
    }

    /// Go's loop body for one item.
    fn note(&mut self, item: &ModelItem) {
        let Some(st) = self.state.as_mut() else {
            // After the end: what an aggregator assembles from the chunks already recorded.
            return;
        };
        let (res, event_id) = item;
        if self.legacy {
            match res {
                Err(e) => {
                    st.result = GenerateContentResult {
                        error: Some(SpanError::new(e)),
                        ..Default::default()
                    };
                    self.end();
                }
                Ok(resp) => {
                    st.result = GenerateContentResult {
                        response: Some(Arc::new(resp.clone())),
                        event_id: event_id.clone(),
                        ..Default::default()
                    };
                    if !resp.partial {
                        // Go logResponse (the legacy schema's gen_ai.choice record) is emitted
                        // here (Task 11).
                        self.end();
                    }
                }
            }
            return;
        }
        let ended = match res {
            Err(e) => {
                st.result.error = Some(SpanError::new(e));
                true
            }
            Ok(resp) => {
                let resp = Arc::new(resp.clone());
                st.result.response = Some(resp.clone());
                st.result.event_id = event_id.clone();
                st.result.responses.push(resp.clone());
                // Partial is set by ADK's aggregator rather than the model; the finish reason
                // comes from the model.
                !reported_finish_reason(Some(&resp), false).is_empty() || !st.params.stream
            }
        };
        if ended {
            self.end();
        }
    }
}

impl Stream for GenerateContentStream {
    type Item = ModelItem;

    fn poll_next(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        let poll = this.inner.as_mut().poll_next(cx);
        match &poll {
            Poll::Ready(Some(item)) => this.note(item),
            Poll::Ready(None) => this.end(),
            Poll::Pending => {}
        }
        poll
    }
}

impl Drop for GenerateContentStream {
    fn drop(&mut self) {
        self.end();
    }
}
