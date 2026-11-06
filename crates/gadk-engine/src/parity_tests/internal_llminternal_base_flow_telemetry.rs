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
use gadk_genai::{FinishReason, GenerateContentResponseUsageMetadata};
use gadk_model::{Llm, LlmRequest, LlmResponse, ModelError};
use pretty_assertions::assert_eq;

use crate::llm::flow::{
    generate_content,
    span_capture::{CapturedSpan, capture_spans, closed_count},
};

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
        while s.next().await.is_some() {
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
        while s.next().await.is_some() {
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

// parity: skipped TestLoggingSpanIDPropagation: OTel log-record export and semconv opt-in env (no OpenTelemetry dependency in 1b, spec R18)
// parity: skipped TestInferenceEventDescribesTheRequestTheSpanDoes: OTel inference log event / message-capture env (no OpenTelemetry dependency in 1b, spec R18)
