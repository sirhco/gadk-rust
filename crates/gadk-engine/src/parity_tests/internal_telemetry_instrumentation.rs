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

//! Ports of ADK-Go `internal/telemetry/instrumentation_test.go`. Spans and log records are
//! captured from `tracing` (plan decision D6).

use std::sync::Arc;

use futures::StreamExt;
use futures::stream::BoxStream;
use gadk_agent::{AgentConfig, AgentError, agent};
use gadk_genai::{Content, FinishReason, GenerateContentResponseUsageMetadata, Part};
use gadk_model::{LlmRequest, LlmResponse};
use gadk_session::Event;
use parking_lot::Mutex;
use pretty_assertions::assert_eq;
use serde_json::Value;
use tracing::Instrument;
use tracing_subscriber::layer::{Context as LayerContext, Layer, SubscriberExt};

use crate::llm::flow::span_capture::capture_spans_in_env;
use crate::telemetry::LOG_TARGET;
use crate::telemetry::attrs::GenerateContentParams;
use crate::telemetry::env::{SCHEMA_VERSION_OPT_IN_ENV_VAR, test_env};
use crate::telemetry::instrumentation::{
    ModelItem, instrument_generate_content, instrument_invocation,
};
use crate::telemetry::span_digest::{capture, pin_tracing_interest};
use crate::telemetry::workflow_span;

/// Go `testAgent`: a non-workflow agent named `root`.
fn test_agent() -> Arc<dyn gadk_agent::Agent> {
    agent::new(AgentConfig {
        name: "root".into(),
        ..Default::default()
    })
    .unwrap()
}

#[tokio::test]
async fn test_instrument_invocation_first_error_and_early_stop() {
    let _env = test_env(&[(SCHEMA_VERSION_OPT_IN_ENV_VAR, "otel_semconv_1_44")]);
    let root = test_agent();
    let ((), spans) = capture_spans_in_env(async {
        let mut s = instrument_invocation(root.as_ref(), "s", || {
            Box::pin(futures::stream::iter([
                Err(AgentError::Config("first".into())),
                Err(AgentError::Config("second".into())),
                Ok(Event::default()),
            ]))
        });
        let mut seen = 0;
        while s.next().await.is_some() {
            seen += 1;
            if seen == 2 {
                break;
            }
        }
    })
    .await;
    assert_eq!(spans.len(), 1, "{spans:#?}");
    assert!(spans[0].closed);
    assert_eq!(spans[0].otel_name, "invoke_workflow root");
    assert_eq!(spans[0].field("error.message").as_deref(), Some("first"));
}

#[tokio::test]
async fn test_instrument_invocation_nested_opens_no_span() {
    let _env = test_env(&[(SCHEMA_VERSION_OPT_IN_ENV_VAR, "otel_semconv_1_44")]);
    let root = test_agent();
    let ((), spans) = capture_spans_in_env(async {
        let outer = workflow_span("outer", "s");
        async {
            let mut s = instrument_invocation(root.as_ref(), "s", || {
                Box::pin(futures::stream::iter([Ok(Event::default())]))
            });
            while s.next().await.is_some() {}
        }
        .instrument(outer)
        .await;
    })
    .await;
    assert_eq!(spans.len(), 1, "want only the outer one: {spans:#?}");
}

fn text_response(text: &str, partial: bool) -> LlmResponse {
    LlmResponse {
        content: Some(Content {
            role: "model".into(),
            parts: vec![Part {
                text: text.into(),
                ..Default::default()
            }],
        }),
        partial,
        ..Default::default()
    }
}

fn scripted(items: Vec<LlmResponse>) -> impl FnOnce(LlmRequest) -> BoxStream<'static, ModelItem> {
    move |_| {
        Box::pin(futures::stream::iter(
            items.into_iter().map(|r| (Ok(r), String::new())),
        ))
    }
}

fn streaming_params() -> GenerateContentParams {
    GenerateContentParams {
        model_name: "m".into(),
        stream: true,
        ..Default::default()
    }
}

/// The levels of the D6 log records `fut` emits.
async fn log_levels<F: Future>(fut: F) -> Vec<tracing::Level> {
    struct Levels(Arc<Mutex<Vec<tracing::Level>>>);
    impl<S: tracing::Subscriber> Layer<S> for Levels {
        fn on_event(&self, event: &tracing::Event<'_>, _: LayerContext<'_, S>) {
            if event.metadata().target() == LOG_TARGET {
                self.0.lock().push(*event.metadata().level());
            }
        }
    }
    pin_tracing_interest();
    let levels: Arc<Mutex<Vec<tracing::Level>>> = Arc::default();
    let sub = tracing_subscriber::registry().with(Levels(levels.clone()));
    {
        let _g = tracing::subscriber::set_default(sub);
        fut.await;
    }
    levels.lock().clone()
}

// parity: adapted: a LogRecord carries no severity, so Go's SeverityDebug is checked as the tracing level of the record's event in a second run
#[tokio::test]
async fn test_instrument_generate_content_early_stop() {
    let _env = test_env(&[(SCHEMA_VERSION_OPT_IN_ENV_VAR, "otel_semconv_1_44")]);
    let run = || async {
        let mut s = instrument_generate_content(
            streaming_params(),
            LlmRequest::default(),
            scripted(vec![
                text_response("par", true),
                text_response("partial", false),
            ]),
        );
        let _ = s.next().await;
    };
    let ((), spans, logs) = capture(run()).await;
    assert_eq!(spans.len(), 1, "{spans:#?}");
    assert_eq!(logs.len(), 1, "{logs:#?}");
    // An abandoned stream reported no finish reason, so none is published.
    assert!(
        !spans[0]
            .attributes
            .iter()
            .any(|(k, _)| k == "gen_ai.response.finish_reasons"),
        "span finish reasons: {:?}",
        spans[0].attributes
    );
    assert!(
        !logs[0]
            .attributes
            .iter()
            .any(|(k, _)| k == "gen_ai.response.finish_reasons"),
        "event finish reasons: {:?}",
        logs[0].attributes
    );
    assert_eq!(log_levels(run()).await, [tracing::Level::DEBUG]);
}

#[tokio::test]
async fn test_instrument_generate_content_usage_from_latest_report() {
    let _env = test_env(&[(SCHEMA_VERSION_OPT_IN_ENV_VAR, "otel_semconv_1_44")]);
    let usage = |n: i32| {
        Some(GenerateContentResponseUsageMetadata {
            prompt_token_count: n,
            ..Default::default()
        })
    };
    let ((), spans, _) = capture(async {
        let mut s = instrument_generate_content(
            streaming_params(),
            LlmRequest::default(),
            scripted(vec![
                LlmResponse {
                    usage_metadata: usage(10),
                    partial: true,
                    ..Default::default()
                },
                LlmResponse {
                    finish_reason: Some(FinishReason::Stop),
                    partial: true,
                    ..Default::default()
                },
                LlmResponse {
                    usage_metadata: usage(99),
                    finish_reason: Some(FinishReason::Stop),
                    ..Default::default()
                },
            ]),
        );
        while s.next().await.is_some() {}
    })
    .await;
    let got = spans[0]
        .attributes
        .iter()
        .find(|(k, _)| k == "gen_ai.usage.input_tokens")
        .map(|(_, v)| v.clone());
    assert_eq!(got, Some(Value::from(10)));
}
