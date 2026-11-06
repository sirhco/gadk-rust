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
}
