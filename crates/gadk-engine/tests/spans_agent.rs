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

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Runner invocation span tests (spec R18). The `invoke_agent`/`invoke_workflow` agent spans come
//! from `CustomAgent::run`; the only span the runner emits is the invocation's `invoke_workflow`.

use std::sync::Arc;

use futures::StreamExt;
use gadk_agent::{AgentConfig, AgentState, AgentType, EventStream, InvocationContext, RunConfig};
use gadk_engine::llm::{LlmAgentConfig, agent as llm_agent};
use gadk_engine::runner::{RunOptions, Runner};
use gadk_engine::testutil::MockModel;
use gadk_genai::{Content, FinishReason, ROLE_MODEL, ROLE_USER};
use gadk_model::{Llm, LlmResponse};
use parking_lot::Mutex;
use tracing::span::{Attributes, Id};
use tracing_subscriber::layer::{Context as LayerContext, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;

/// Installs (once per test binary) a no-op global registry so callsite interest never collapses
/// to "off" while thread-local capture subscribers come and go on parallel test threads (the
/// external-test equivalent of the lib's `pin_tracing_interest`; external test binaries cannot
/// reach the lib's `#[cfg(test)]` helper).
fn pin_tracing_interest() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = tracing::subscriber::set_global_default(tracing_subscriber::registry());
    });
}

#[derive(Clone, Debug)]
struct SpanRec {
    name: &'static str,
    otel_name: String,
    ancestors: Vec<String>,
}

struct OtelName(String);

#[derive(Default)]
struct OtelVisitor(Option<String>);

impl tracing::field::Visit for OtelVisitor {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "otel.name" {
            self.0 = Some(value.to_string());
        }
    }
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "otel.name" {
            self.0 = Some(format!("{value:?}").trim_matches('"').to_string());
        }
    }
}

#[derive(Clone, Default)]
struct SpanLog(Arc<Mutex<Vec<SpanRec>>>);

impl<S> tracing_subscriber::Layer<S> for SpanLog
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: LayerContext<'_, S>) {
        let mut v = OtelVisitor::default();
        attrs.record(&mut v);
        let otel_name = v.0.unwrap_or_default();
        let span = ctx.span(id).unwrap();
        // Ancestry is recorded at creation, so span-ID reuse cannot confuse it.
        let ancestors = span
            .scope()
            .skip(1)
            .filter_map(|s| s.extensions().get::<OtelName>().map(|o| o.0.clone()))
            .collect();
        span.extensions_mut().insert(OtelName(otel_name.clone()));
        self.0.lock().push(SpanRec {
            name: attrs.metadata().name(),
            otel_name,
            ancestors,
        });
    }
}

/// Runs `root` once through `Runner::in_memory` with a thread-local span log (current-thread
/// runtime only, so every spawned task runs on this thread).
async fn capture(root: Arc<dyn gadk_agent::Agent>) -> Vec<SpanRec> {
    pin_tracing_interest();
    let log = SpanLog::default();
    let _g = tracing::subscriber::set_default(tracing_subscriber::registry().with(log.clone()));
    let runner = Runner::in_memory("app", root).unwrap();
    let items: Vec<_> = runner
        .run(
            "u",
            "s",
            Some(Content::from_text("hi", ROLE_USER)),
            RunConfig::default(),
            RunOptions::default(),
        )
        .collect()
        .await;
    assert!(items.iter().all(|i| i.is_ok()), "{items:?}");
    log.0.lock().clone()
}

fn empty_run() -> gadk_agent::AgentRunFn {
    Arc::new(|_ctx: InvocationContext| -> EventStream { Box::pin(futures::stream::empty()) })
}

fn with_otel<'a>(spans: &'a [SpanRec], otel: &str) -> Vec<&'a SpanRec> {
    spans.iter().filter(|s| s.otel_name == otel).collect()
}

#[tokio::test(flavor = "current_thread")]
async fn runner_wraps_invocation_in_invoke_workflow_span() {
    let root = gadk_agent::new_agent(AgentConfig {
        name: "solo".into(),
        run: Some(empty_run()),
        ..Default::default()
    })
    .unwrap();
    let spans = capture(root).await;
    let wf = with_otel(&spans, "invoke_workflow solo");
    assert_eq!(wf.len(), 1, "{spans:#?}");
    assert_eq!(wf[0].name, "invoke_workflow");
    let agent = with_otel(&spans, "invoke_agent solo");
    assert_eq!(agent.len(), 1, "{spans:#?}");
    assert_eq!(agent[0].name, "invoke_agent");
    assert!(
        agent[0]
            .ancestors
            .iter()
            .any(|a| a == "invoke_workflow solo"),
        "{spans:#?}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn runner_skips_invocation_span_for_workflow_agent_root() {
    let root = gadk_agent::new_with_state(
        AgentConfig {
            name: "wf".into(),
            run: Some(empty_run()),
            ..Default::default()
        },
        AgentState {
            agent_type: AgentType::WorkflowAgent,
            config: None,
            on_before_agent_callback_event: None,
        },
    )
    .unwrap();
    let spans = capture(root).await;
    assert_eq!(
        with_otel(&spans, "invoke_workflow wf").len(),
        1,
        "only the agent's own span: {spans:#?}"
    );
    assert!(
        with_otel(&spans, "invoke_agent wf").is_empty(),
        "{spans:#?}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn llm_root_runs_without_invoke_node_span() {
    let model: Arc<dyn Llm> = MockModel::new(vec![Ok(LlmResponse {
        content: Some(Content::from_text("hi", ROLE_MODEL)),
        finish_reason: Some(FinishReason::Stop),
        ..Default::default()
    })]);
    let root = llm_agent::new(LlmAgentConfig {
        name: "assistant".into(),
        model: Some(model),
        ..Default::default()
    })
    .unwrap();
    let spans = capture(root).await;
    let agent = with_otel(&spans, "invoke_agent assistant");
    assert_eq!(agent.len(), 1, "{spans:#?}");
    assert!(
        agent[0]
            .ancestors
            .iter()
            .any(|a| a == "invoke_workflow assistant"),
        "{spans:#?}"
    );
    assert!(
        with_otel(&spans, "invoke_node assistant").is_empty(),
        "the runner node sets emits_own_span: {spans:#?}"
    );
}
