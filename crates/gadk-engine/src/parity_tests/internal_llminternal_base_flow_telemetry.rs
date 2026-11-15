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

//! Parity port of Go internal/llminternal/base_flow_telemetry_test.go (OTel spans → tracing spans).

use std::{any::Any, sync::Arc};

use futures::{StreamExt, stream::BoxStream};
use gadk_agent::InvocationContext;
use gadk_genai::{
    Content, FinishReason, GenerateContentConfig, GenerateContentResponseUsageMetadata, Part,
};
use gadk_model::{Llm, LlmRequest, LlmResponse, ModelError};
use pretty_assertions::assert_eq;

use crate::llm::flow::{
    generate_content,
    span_capture::{CapturedSpan, capture_spans, closed_count},
};
use crate::telemetry::env::{
    CAPTURE_MESSAGE_CONTENT_ENV_VAR, SCHEMA_VERSION_OPT_IN_ENV_VAR, test_env,
};
use crate::telemetry::span_digest::capture;

/// Go mockModelForTest with a scripted stream.
struct ScriptModel(parking_lot::Mutex<Option<Vec<Result<LlmResponse, ModelError>>>>);

impl ScriptModel {
    fn new(items: Vec<Result<LlmResponse, ModelError>>) -> Arc<Self> {
        Arc::new(Self(parking_lot::Mutex::new(Some(items))))
    }
}

impl Llm for ScriptModel {
    fn name(&self) -> &str {
        "test-model"
    }
    fn generate_content(
        &self,
        _: LlmRequest,
        _: bool,
    ) -> BoxStream<'static, Result<LlmResponse, ModelError>> {
        Box::pin(futures::stream::iter(
            self.0.lock().take().unwrap_or_default(),
        ))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn usage(p: i32, c: i32) -> Option<GenerateContentResponseUsageMetadata> {
    Some(GenerateContentResponseUsageMetadata {
        prompt_token_count: p,
        candidates_token_count: c,
        ..Default::default()
    })
}

fn only_generate_span(spans: &[CapturedSpan]) -> &CapturedSpan {
    let g: Vec<_> = spans
        .iter()
        .filter(|s| s.name == "generate_content")
        .collect();
    assert_eq!(
        g.len(),
        1,
        "expected 1 generate_content span, got {}",
        g.len()
    );
    g[0]
}

// parity: adapted: OTel span attributes become fields of a tracing event recorded inside the "generate_content" span (otel.name "generate_content test-model"); end-of-span is the span close
#[tokio::test]
async fn test_generate_content_tracing() {
    let m = ScriptModel::new(vec![
        Ok(LlmResponse {
            usage_metadata: usage(1, 2),
            ..Default::default()
        }),
        Ok(LlmResponse {
            usage_metadata: usage(10, 20),
            finish_reason: Some(FinishReason::Stop),
            ..Default::default()
        }),
        Ok(LlmResponse {
            usage_metadata: usage(100, 200),
            ..Default::default()
        }),
    ]);
    let ctx = InvocationContext::builder().build_invocation();
    let (closed_after, spans) = capture_spans(async {
        let mut s = generate_content(&ctx, m, LlmRequest::default(), true);
        let mut closed_after = Vec::new();
        while let Some((item, _)) = s.next().await {
            if let Err(e) = item {
                panic!("generateContent() error = {e}");
            }
            closed_after.push(closed_count("generate_content"));
        }
        closed_after
    })
    .await;
    // Go: not ended after chunk 1; ended after chunk 2 (finish reason); no new span after chunk 3.
    assert_eq!(closed_after, vec![0, 1, 1]);
    let span = only_generate_span(&spans);
    assert!(span.closed);
    assert_eq!(span.otel_name, "generate_content test-model");
    assert_eq!(
        span.field("gen_ai.usage.input_tokens").as_deref(),
        Some("10")
    );
    assert_eq!(
        span.field("gen_ai.usage.output_tokens").as_deref(),
        Some("20")
    );
}

// parity: adapted: as test_generate_content_tracing; Go's trailing nil response cannot occur in Rust, so the stream simply ends after the partial chunk
#[tokio::test]
async fn test_generate_content_tracing_no_final_response() {
    let m = ScriptModel::new(vec![Ok(LlmResponse {
        usage_metadata: usage(10, 20),
        partial: true,
        ..Default::default()
    })]);
    let ctx = InvocationContext::builder().build_invocation();
    let (closed_after, spans) = capture_spans(async {
        let mut s = generate_content(&ctx, m, LlmRequest::default(), true);
        let mut closed_after = Vec::new();
        while let Some((item, _)) = s.next().await {
            if let Err(e) = item {
                panic!("generateContent() error = {e}");
            }
            closed_after.push(closed_count("generate_content"));
        }
        closed_after
    })
    .await;
    // Not ended after the partial chunk; ended with the stream.
    assert_eq!(closed_after, vec![0]);
    let span = only_generate_span(&spans);
    assert!(span.closed);
    assert_eq!(span.otel_name, "generate_content test-model");
    assert_eq!(
        span.field("gen_ai.usage.input_tokens").as_deref(),
        Some("10")
    );
    assert_eq!(
        span.field("gen_ai.usage.output_tokens").as_deref(),
        Some("20")
    );
}

// parity: adapted: OTel span status becomes the span's otel.status_code field plus an error event carrying otel.status_description
#[tokio::test]
async fn test_generate_content_tracing_error() {
    let m = ScriptModel::new(vec![
        Ok(LlmResponse {
            usage_metadata: usage(1, 2),
            partial: true,
            ..Default::default()
        }),
        Err(ModelError::Other("test error".into())),
    ]);
    let ctx = InvocationContext::builder().build_invocation();
    let (closed_after, spans) = capture_spans(async {
        let mut s = generate_content(&ctx, m, LlmRequest::default(), true);
        let mut closed_after = Vec::new();
        while let Some((item, _)) = s.next().await {
            closed_after.push((item.is_err(), closed_count("generate_content")));
        }
        closed_after
    })
    .await;
    // The span is closed by the time the error item is yielded.
    assert_eq!(closed_after, vec![(false, 0), (true, 1)]);
    let span = only_generate_span(&spans);
    assert_eq!(span.otel_name, "generate_content test-model");
    assert_eq!(span.field("otel.status_code").as_deref(), Some("ERROR"));
    assert_eq!(
        span.field("otel.status_description").as_deref(),
        Some("test error")
    );
}

/// Go `mockModelForTest` with a `generateContent` hook: `on_call` sees the request (and the
/// current span) when the model is called, then the model yields `resp`.
struct HookModel {
    on_call: Box<dyn Fn(&LlmRequest) + Send + Sync>,
    resp: LlmResponse,
}

impl Llm for HookModel {
    fn name(&self) -> &str {
        "test-model"
    }
    fn generate_content(
        &self,
        req: LlmRequest,
        _: bool,
    ) -> BoxStream<'static, Result<LlmResponse, ModelError>> {
        (self.on_call)(&req);
        Box::pin(futures::stream::iter([Ok(self.resp.clone())]))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn text(role: &str, t: &str) -> Content {
    Content {
        role: role.into(),
        parts: vec![Part {
            text: t.into(),
            ..Default::default()
        }],
    }
}

// parity: adapted: a captured LogRecord's span ID is the capture's index of the span it was emitted under, so the span the model sees is identified by name (the one generate_content span) instead of its OTel span ID
#[tokio::test]
async fn test_logging_span_id_propagation() {
    let _env = test_env(&[(SCHEMA_VERSION_OPT_IN_ENV_VAR, "otel_semconv_1_44")]);
    let seen = Arc::new(parking_lot::Mutex::new(Vec::<String>::new()));
    let model = Arc::new(HookModel {
        on_call: Box::new({
            let seen = seen.clone();
            move |_| {
                // Capture the span the model is called in.
                let name = tracing::Span::current()
                    .metadata()
                    .map_or("", |m| m.name())
                    .to_owned();
                seen.lock().push(name);
            }
        }),
        resp: LlmResponse {
            usage_metadata: usage(1, 2),
            content: Some(text("model", "Response")),
            ..Default::default()
        },
    });
    let req = LlmRequest {
        config: Some(GenerateContentConfig {
            system_instruction: Some(text("system", "You are a helpful assistant.")),
            ..Default::default()
        }),
        contents: vec![text("user", "Hello")],
        ..Default::default()
    };
    let ctx = InvocationContext::builder().build_invocation();
    let ((), spans, logs) = capture(async {
        let mut s = generate_content(&ctx, model, req, true);
        while let Some((item, _)) = s.next().await {
            if let Err(e) = item {
                panic!("generateContent() error = {e}");
            }
        }
    })
    .await;
    assert_eq!(
        *seen.lock(),
        ["generate_content"],
        "the model runs inside the span"
    );
    let generate: Vec<_> = spans
        .iter()
        .filter(|s| s.name == "generate_content test-model")
        .collect();
    assert_eq!(generate.len(), 1, "{spans:#?}");
    let want_span_id = Some(generate[0].span_id);
    let want_events = ["gen_ai.client.inference.operation.details"];
    assert_eq!(
        logs.len(),
        want_events.len(),
        "expected {} log records, got {logs:#?}",
        want_events.len()
    );
    for (i, record) in logs.iter().enumerate() {
        assert_eq!(record.span_id, want_span_id, "record[{i}]: span ID");
        assert_eq!(record.event_name, want_events[i], "record[{i}]: event name");
    }
}

/// The number of messages in `gen_ai.input.messages` among `attrs`.
fn input_messages(attrs: &[(String, serde_json::Value)]) -> usize {
    attrs
        .iter()
        .find(|(k, _)| k == "gen_ai.input.messages")
        .and_then(|(_, v)| v.as_array())
        .map_or(0, Vec::len)
}

// parity: adapted: Go's model appends a message to the request it was handed and the span and the event both describe the edited request; a Rust model receives the request by value and cannot edit the caller's, so the span and the event both describe the request the model received (one message)
#[tokio::test]
async fn test_inference_event_describes_the_request_the_span_does() {
    let _env = test_env(&[
        (SCHEMA_VERSION_OPT_IN_ENV_VAR, "otel_semconv_1_44"),
        (CAPTURE_MESSAGE_CONTENT_ENV_VAR, "SPAN_AND_EVENT"),
    ]);
    let received = Arc::new(parking_lot::Mutex::new(0usize));
    let model = Arc::new(HookModel {
        on_call: Box::new({
            let received = received.clone();
            move |req| *received.lock() = req.contents.len()
        }),
        resp: LlmResponse {
            content: Some(text("model", "Response")),
            ..Default::default()
        },
    });
    let req = LlmRequest {
        contents: vec![text("user", "Hello")],
        ..Default::default()
    };
    let ctx = InvocationContext::builder().build_invocation();
    let ((), spans, logs) = capture(async {
        let mut s = generate_content(&ctx, model, req, false);
        while let Some((item, _)) = s.next().await {
            if let Err(e) = item {
                panic!("generateContent() error = {e}");
            }
        }
    })
    .await;
    let want = *received.lock();
    assert_eq!(want, 1);
    assert_eq!(
        input_messages(&spans[0].attributes),
        want,
        "span gen_ai.input.messages"
    );
    assert_eq!(logs.len(), 1, "expected 1 log record, got {logs:#?}");
    assert_eq!(
        input_messages(&logs[0].attributes),
        want,
        "event gen_ai.input.messages"
    );
}
