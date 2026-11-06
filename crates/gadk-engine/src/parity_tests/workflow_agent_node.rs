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

//! Port of Go workflow/agent_node_test.go.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use futures::StreamExt;
use gadk_agent::{AgentConfig, AgentError, EventStream, InvocationContext};
use gadk_genai::{Content, Part};
use gadk_platform::Platform;
use gadk_session::Event;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::tool::utils::infer_schema;
use crate::workflow::nodes::agent::node_input_to_content;
use crate::workflow::scheduler::test_support::*;
use crate::workflow::{AgentNode, Node, NodeConfig, Workflow, WorkflowOptions, function_node};

#[derive(Serialize, Deserialize, schemars::JsonSchema)]
#[allow(dead_code)]
struct Input {
    value: String,
}
#[derive(Serialize, Deserialize, schemars::JsonSchema)]
#[allow(dead_code)]
struct Output {
    result: String,
}

/// Port of Go `agent.New(agent.Config{Name, Description, Run})`.
fn custom_agent(
    name: &str,
    description: &str,
    run: impl Fn(InvocationContext) -> EventStream + Send + Sync + 'static,
) -> Arc<dyn gadk_agent::Agent> {
    gadk_agent::new_agent(AgentConfig {
        name: name.into(),
        description: description.into(),
        run: Some(Arc::new(run)),
        ..AgentConfig::default()
    })
    .expect("failed to create agent")
}

/// The agent echoes its user content's first text part (Go `uc.Parts[0].Text`, "Unknown" if none).
fn echo_text(ctx: &InvocationContext) -> String {
    ctx.user_content()
        .and_then(|uc| uc.parts.first())
        .map(|p| p.text.clone())
        .unwrap_or_else(|| "Unknown".into())
}

/// Port of Go `newMockCtx` + `mockCtx.sess = &mockSession{..}` + optional isolation scope.
fn mock_node_ctx(scope: &str) -> gadk_agent::Context {
    InvocationContext::builder()
        .invocation_id(TEST_INVOCATION_ID)
        .platform(Platform::deterministic(1))
        .isolation_scope(scope)
        .build()
}

fn model_event(ctx: &InvocationContext, parts: Vec<Part>) -> Event {
    let mut ev = Event::new(ctx.platform(), ctx.invocation_id());
    ev.response.content = Some(Content {
        role: "model".into(),
        parts,
    });
    ev
}

// parity: adapted: Go's generic NewAgentNodeTyped has no Rust counterpart (Appendix C); the typed case uses AgentNode::with_schemas with inferred schemas.
#[tokio::test]
async fn test_agent_node_new() {
    let my_agent = custom_agent("test_agent", "a test agent", |ctx| {
        let mut ev = Event::new(ctx.platform(), ctx.invocation_id());
        ev.output = Some(json!({"result": "success"}));
        once(ev)
    });
    let creators: Vec<(&str, Arc<AgentNode>)> = vec![
        (
            "NewAgentNodeTyped",
            AgentNode::with_schemas(
                Arc::clone(&my_agent),
                Some(infer_schema::<Input>()),
                Some(infer_schema::<Output>()),
                NodeConfig::default(),
            )
            .expect("creation failed"),
        ),
        (
            "NewAgentNodeWithSchemas",
            AgentNode::with_schemas(
                Arc::clone(&my_agent),
                Some(infer_schema::<Input>()),
                Some(infer_schema::<Output>()),
                NodeConfig::default(),
            )
            .expect("creation failed"),
        ),
        (
            "NewAgentNode",
            AgentNode::new(Arc::clone(&my_agent), NodeConfig::default()).expect("creation failed"),
        ),
    ];
    for (case, node) in creators {
        assert_eq!(node.name(), "test_agent", "{case}: node.Name()");
        assert_eq!(
            node.description(),
            "a test agent",
            "{case}: node.Description()"
        );
        assert!(
            node.input_schema().is_some() && node.output_schema().is_some(),
            "{case}: expected schemas to be resolved"
        );
    }
}

// parity: adapted: struct inputs are JSON values; NewAgentNodeTyped → with_schemas(inferred).
#[rstest::rstest]
#[case::struct_input_output("struct_input_output")]
#[case::string_output("string_output")]
#[case::agent_execution_error("agent_execution_error")]
#[tokio::test]
async fn test_agent_node_run(#[case] case: &'static str) {
    let my_agent = custom_agent("test_agent", "", move |ctx| match case {
        "agent_execution_error" => fail(AgentError::other("something went wrong")),
        "string_output" => {
            let mut ev = Event::new(ctx.platform(), ctx.invocation_id());
            ev.output = Some(Value::String(echo_text(&ctx)));
            once(ev)
        }
        _ => {
            let mut ev = Event::new(ctx.platform(), ctx.invocation_id());
            ev.output = Some(json!({"result": echo_text(&ctx)}));
            once(ev)
        }
    });
    let output_schema = if case == "string_output" {
        json!({"type": "string"})
    } else {
        infer_schema::<Output>()
    };
    let node = AgentNode::with_schemas(
        my_agent,
        Some(json!({"type": "object", "properties": {"val": {"type": "string"}}})),
        Some(output_schema),
        NodeConfig::default(),
    )
    .expect("node creation failed");
    let input = json!({"val": match case {
        "string_output" => "B",
        "agent_execution_error" => "C",
        _ => "A",
    }});
    let mut s = node.run(top_level_ctx(), input);
    let mut got = Vec::new();
    while let Some(item) = s.next().await {
        match item {
            Err(e) => {
                assert_eq!(case, "agent_execution_error", "unexpected error: {e}");
                assert!(e.to_string().contains("something went wrong"), "got: {e}");
                return;
            }
            Ok(ev) => got.push(ev.output.unwrap_or(Value::Null)),
        }
    }
    assert_ne!(
        case, "agent_execution_error",
        "expected at least one event/error from Run"
    );
    assert_eq!(got.len(), 1, "expected 1 event");
    let text = match case {
        "string_output" => got[0].as_str().map(str::to_string),
        _ => got[0]
            .get("result")
            .and_then(Value::as_str)
            .map(str::to_string),
    };
    let want = if case == "string_output" {
        r#"{"val":"B"}"#
    } else {
        r#"{"val":"A"}"#
    };
    assert_eq!(text.as_deref(), Some(want));
}

#[derive(Serialize, Deserialize, schemars::JsonSchema)]
struct IntInput {
    val: i64,
}
#[derive(Serialize, Deserialize, schemars::JsonSchema)]
struct IntOutput {
    result: i64,
}

// parity: adapted: NewAgentNodeTyped[Input, Output] → AgentNode::with_schemas(inferred).
#[rstest::rstest]
#[case::chain_agent_and_function(5, 11)]
#[case::chain_agent_and_function_zero(0, 1)]
#[tokio::test]
async fn test_agent_node_workflow_integration(#[case] input: i64, #[case] want: i64) {
    let integration_agent = custom_agent("integration_agent", "", |ctx| {
        // The user content is the JSON text of the seed output, e.g. `{"val":5}`.
        let val_str = ctx
            .user_content()
            .and_then(|uc| uc.parts.first())
            .map(|p| p.text.clone())
            .unwrap_or_else(|| "0".into());
        let val = serde_json::from_str::<IntInput>(&val_str).map_or(0, |p| p.val);
        let mut ev = Event::new(ctx.platform(), ctx.invocation_id());
        ev.output = Some(json!({"result": val * 2}));
        once(ev)
    });
    let agent_node: Arc<dyn Node> = AgentNode::with_schemas(
        integration_agent,
        Some(infer_schema::<IntInput>()),
        Some(infer_schema::<IntOutput>()),
        NodeConfig::default(),
    )
    .expect("NewAgentNodeTyped failed");
    let function_node_ = function_node::<IntOutput, i64, _, _>(
        "plus_one",
        |_c, i: IntOutput| async move { Ok(i.result + 1) },
        NodeConfig::default(),
    );
    let seed = function_node::<Value, IntInput, _, _>(
        "seed",
        move |_c, _i: Value| async move { Ok(IntInput { val: input }) },
        NodeConfig::default(),
    );
    let w = Workflow::new(
        "test_workflow",
        chain(&[&start(), &seed, &agent_node, &function_node_]),
        WorkflowOptions::default(),
    )
    .expect("unexpected error");
    let events = drain(w.run(mock_ctx())).await;
    let out = events.iter().filter_map(|e| e.output.clone()).last();
    assert_eq!(out, Some(json!(want)), "output mismatch");
}

#[tokio::test]
async fn test_agent_node_synthesizes_output_from_model_text() {
    let wrapped = custom_agent("talky", "", |ctx| {
        // Partial must not be promoted to Output.
        let mut partial = model_event(&ctx, vec![Part::from_text("Hel")]);
        partial.response.partial = true;
        // Thought parts are skipped; text parts concatenate.
        let thought = Part {
            thought: true,
            ..Part::from_text("thinking…")
        };
        let fin = model_event(
            &ctx,
            vec![
                thought,
                Part::from_text("Hello, "),
                Part::from_text("world!"),
            ],
        );
        futures::stream::iter([Ok(partial), Ok(fin)]).boxed()
    });
    let node = AgentNode::new(wrapped, NodeConfig::default()).expect("NewAgentNode");
    let events = drain(node.run(mock_node_ctx(""), json!("ignored"))).await;
    let got_partial = events
        .iter()
        .find(|e| e.response.partial)
        .expect("missing partial event");
    let got_final = events
        .iter()
        .find(|e| !e.response.partial)
        .expect("missing final event");
    assert_eq!(
        got_partial.output, None,
        "partials must not be promoted to Output"
    );
    assert_eq!(got_final.output, Some(json!("Hello, world!")));
    assert!(
        got_final
            .node_info
            .as_ref()
            .is_some_and(|i| i.message_as_output),
        "final event NodeInfo.MessageAsOutput = {:?}, want true",
        got_final.node_info
    );
    assert!(
        !got_partial
            .node_info
            .as_ref()
            .is_some_and(|i| i.message_as_output),
        "partial event MessageAsOutput = true, want false/unset"
    );
}

#[tokio::test]
async fn test_agent_node_stamps_isolation_scope_on_events() {
    let got_agent_scope = Arc::new(Mutex::new(String::new()));
    let g = Arc::clone(&got_agent_scope);
    let wrapped = custom_agent("scoped", "", move |ctx| {
        *g.lock() = ctx.isolation_scope();
        let mut ev = Event::new(ctx.platform(), ctx.invocation_id());
        ev.output = Some(json!("v"));
        // An event that already carries a scope is left untouched.
        let mut pre = Event::new(ctx.platform(), ctx.invocation_id());
        pre.isolation_scope = "preset".into();
        futures::stream::iter([Ok(ev), Ok(pre)]).boxed()
    });
    let node = AgentNode::new(wrapped, NodeConfig::default()).expect("NewAgentNode");
    let events = drain(node.run(mock_node_ctx("scope-x"), json!("ignored"))).await;
    assert_eq!(
        *got_agent_scope.lock(),
        "scope-x",
        "agent ctx IsolationScope"
    );
    assert_eq!(events.len(), 2, "got {} events, want 2", events.len());
    assert_eq!(
        events[0].isolation_scope, "scope-x",
        "event[0] IsolationScope"
    );
    assert_eq!(
        events[1].isolation_scope, "preset",
        "event[1] IsolationScope (preset must be kept)"
    );
}

/// An agent, whether it ran, and the user content it saw.
type RecordingAgent = (
    Arc<dyn gadk_agent::Agent>,
    Arc<AtomicBool>,
    Arc<Mutex<Option<Content>>>,
);

/// An agent "test_agent" that records whether it ran and the user content it saw.
fn recording_agent() -> RecordingAgent {
    let invoked = Arc::new(AtomicBool::new(false));
    let content = Arc::new(Mutex::new(None));
    let (i, c) = (Arc::clone(&invoked), Arc::clone(&content));
    let a = custom_agent("test_agent", "", move |ctx| {
        i.store(true, Ordering::SeqCst);
        *c.lock() = ctx.user_content().cloned();
        let mut ev = Event::new(ctx.platform(), ctx.invocation_id());
        ev.output = Some(json!("ok"));
        once(ev)
    });
    (a, invoked, content)
}

#[derive(Serialize, Deserialize, schemars::JsonSchema)]
struct CustomStruct {
    val: String,
}

// parity: adapted: NewAgentNodeTyped[T, string] → AgentNode::with_schemas(inferred T, string); struct inputs are JSON values.
#[rstest::rstest]
#[case::struct_input_into_json_text_part(json!({"val": "test-val"}), infer_schema::<CustomStruct>(), r#"{"val":"test-val"}"#)]
#[case::string_input_into_text_part(json!("direct string input"), json!({"type": "string"}), "direct string input")]
#[tokio::test]
async fn test_agent_node_validation_and_content_conversion(
    #[case] node_input: Value,
    #[case] input_schema: Value,
    #[case] want_text: &str,
) {
    let (my_agent, invoked, got_content) = recording_agent();
    let node = AgentNode::with_schemas(
        my_agent,
        Some(input_schema),
        Some(json!({"type": "string"})),
        NodeConfig::default(),
    )
    .expect("failed to create node");
    drain(node.run(mock_node_ctx(""), node_input)).await;
    assert!(invoked.load(Ordering::SeqCst), "agent was not invoked");
    let got = got_content
        .lock()
        .clone()
        .expect("expected user content, got nil");
    assert_eq!(
        got,
        Content::from_text(want_text, "user"),
        "user content mismatch"
    );
}

// parity: adapted: Rust has no typed-nil *genai.Content; the nil input (Value::Null) is the counterpart.
#[test]
fn test_node_input_to_content_typed_nil_content() {
    let got = node_input_to_content(&Value::Null).expect("nodeInputToContent returned error");
    assert_eq!(got, None, "nodeInputToContent(nil) = {got:?}, want nil");
}

// parity: adapted: NewAgentNodeTyped[T, string] → AgentNode::with_schemas(inferred T, string).
#[rstest::rstest]
#[case::validation_failure_scheduler_rejects(json!({"type": "string"}), json!({"val": "invalid"}), true)]
#[case::validation_success_agent_invoked(infer_schema::<CustomStruct>(), json!({"val": "hello"}), false)]
#[tokio::test]
async fn test_agent_node_input_validation(
    #[case] input_schema: Value,
    #[case] node_input: Value,
    #[case] want_err: bool,
) {
    let (my_agent, invoked, got_content) = recording_agent();
    let node: Arc<dyn Node> = AgentNode::with_schemas(
        my_agent,
        Some(input_schema),
        Some(json!({"type": "string"})),
        NodeConfig::default(),
    )
    .expect("failed to create node");
    let w = Workflow::new(
        "validation_wf",
        chain(&[&start(), &node]),
        WorkflowOptions::default(),
    )
    .expect("failed to build workflow");
    let (_, workflow_err) = collect(w.run_node(mock_node_ctx(""), node_input)).await;
    if want_err {
        let err = workflow_err.expect("expected validation error, got nil");
        assert!(
            is_kind(&err, crate::error::WorkflowErrorKind::InputValidation),
            "expected ErrInputValidation, got: {err}"
        );
        assert!(
            !invoked.load(Ordering::SeqCst),
            "agent was invoked despite validation failure"
        );
        return;
    }
    assert!(workflow_err.is_none(), "workflow failed: {workflow_err:?}");
    assert!(invoked.load(Ordering::SeqCst), "agent was not invoked");
    let got = got_content
        .lock()
        .clone()
        .expect("expected user content, got nil");
    assert_eq!(
        got,
        Content::from_text(r#"{"val":"hello"}"#, "user"),
        "user content mismatch"
    );
}

#[tokio::test]
async fn test_agent_node_structured_output_projected_via_validation() {
    let wrapped = custom_agent("json-talky", "", |ctx| {
        once(model_event(
            &ctx,
            vec![Part::from_text(r#"{"value":"hello"}"#)],
        ))
    });
    let node = AgentNode::with_schemas(
        wrapped,
        None,
        Some(infer_schema::<Input>()),
        NodeConfig::default(),
    )
    .expect("NewAgentNodeWithSchemas");
    let events = drain(node.run(mock_node_ctx(""), json!("ignored"))).await;
    let got_final = events
        .iter()
        .rfind(|e| !e.response.partial)
        .expect("missing final event");
    // AgentNode itself only synthesizes the raw text; the projection onto the schema happens in
    // ValidateOutput.
    let got = node
        .validate_output(got_final.output.clone().unwrap_or(Value::Null))
        .expect("ValidateOutput");
    assert!(
        got.is_object(),
        "ValidateOutput returned {got}, want an object"
    );
    assert_eq!(got["value"], json!("hello"), "got {got}, want value=hello");
}

#[tokio::test]
async fn test_agent_node_automatic_output_extraction() {
    let my_agent = custom_agent("text_only_agent", "", |ctx| {
        // Model response with plain text, but no Output set.
        once(model_event(
            &ctx,
            vec![
                Part::from_text("This is "),
                Part::from_text("the output text."),
            ],
        ))
    });
    let node = AgentNode::new(my_agent, NodeConfig::default()).expect("failed to create AgentNode");
    let events = drain(node.run(mock_node_ctx(""), Value::Null)).await;
    let final_output = events.iter().filter_map(|e| e.output.clone()).last();
    assert_eq!(
        final_output,
        Some(json!("This is the output text.")),
        "automatically extracted output"
    );
}
