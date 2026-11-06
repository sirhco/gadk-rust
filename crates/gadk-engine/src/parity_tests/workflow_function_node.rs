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

//! Port of Go workflow/function_node_test.go.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use futures::StreamExt;
use gadk_agent::{AgentError, Context, InvocationContext};
use gadk_genai::{Content, Part};
use gadk_session::{Event, RequestInput};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::error::{WorkflowError, WorkflowErrorKind};
use crate::tool::utils::infer_schema;
use crate::workflow::scheduler::Scheduler;
use crate::workflow::scheduler::test_support::*;
use crate::workflow::state::NodeStatus;
use crate::workflow::{
    Emit, Node, NodeConfig, Workflow, WorkflowOptions, emitting_function_node,
    emitting_function_node_with_schema, function_node, function_node_from_state,
    function_node_with_schema, new_request_input_event,
};

#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
struct Input {
    value: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[allow(dead_code)]
struct Output {
    result: String,
}

#[derive(schemars::JsonSchema)]
#[allow(dead_code)]
struct TargetOutput {
    result: i64,
}

#[tokio::test]
async fn test_new_function_node_with_schema() {
    // Case "Success".
    let upper = function_node_with_schema::<Input, Value, _, _>(
        "upper",
        |_c: Context, input: Input| async move { Ok(json!({"result": input.value.to_uppercase()})) },
        Some(infer_schema::<Input>()),
        Some(infer_schema::<Output>()),
        NodeConfig::default(),
    )
    .expect("NewFunctionNodeWithSchema failed");
    let events = drain(upper.run(top_level_ctx(), json!({"value": "hello"}))).await;
    assert_eq!(events.len(), 1, "expected 1 event");
    assert_eq!(events[0].output, Some(json!({"result": "HELLO"})));

    // Case "NilInput": a nil input reaches the function as the zero Input.
    let nil_test = function_node_with_schema::<Input, Value, _, _>(
        "nil_test",
        |_c: Context, input: Input| async move {
            Ok(if input.value.is_empty() {
                json!({"result": "zero"})
            } else {
                json!({"result": "not-zero"})
            })
        },
        Some(infer_schema::<Input>()),
        Some(infer_schema::<Output>()),
        NodeConfig::default(),
    )
    .expect("NewFunctionNodeWithSchema failed");
    let events = drain(nil_test.run(top_level_ctx(), Value::Null)).await;
    assert_eq!(events.len(), 1, "expected 1 event");
    assert_eq!(events[0].output, Some(json!({"result": "zero"})));
}

#[tokio::test]
async fn test_function_node_run_does_not_validate() {
    let raw = json!({"result": "not-an-int"});
    let r = raw.clone();
    let node = function_node_with_schema::<Input, Value, _, _>(
        "test",
        move |_c: Context, _i: Input| {
            let r = r.clone();
            async move { Ok(r) }
        },
        Some(infer_schema::<Input>()),
        Some(infer_schema::<TargetOutput>()),
        NodeConfig::default(),
    )
    .expect("NewFunctionNodeWithSchema failed");
    let events = drain(node.run(top_level_ctx(), json!({"value": "hello"}))).await;
    assert_eq!(events.len(), 1, "expected 1 event from Run");
    assert_eq!(
        events[0].output,
        Some(raw),
        "Run must yield the raw output unvalidated"
    );
}

#[tokio::test]
async fn test_function_node_direct_event_propagation() {
    let node = function_node::<String, Event, _, _>(
        "event_proc",
        |ctx: Context, input: String| async move {
            let mut ev = Event::new(ctx.platform(), ctx.invocation_id());
            ev.output = Some(json!(format!("{input} processed")));
            ev.routes = vec!["CUSTOM_ROUTE".to_string()];
            Ok(ev)
        },
        NodeConfig::default(),
    );
    let events = drain(node.run(top_level_ctx(), json!("hello"))).await;
    assert_eq!(events.len(), 1, "expected 1 event");
    assert_eq!(events[0].output, Some(json!("hello processed")));
    assert_eq!(events[0].routes, vec!["CUSTOM_ROUTE".to_string()]);
}

#[tokio::test]
async fn test_function_node_content_output_goes_to_content() {
    // Go's "pointer *genai.Content" and "value genai.Content" sub-cases are one Rust case (Content by value).
    let want = Content::from_text("hello from content", "model");
    let w = want.clone();
    let node = function_node::<Value, Content, _, _>(
        "content_out",
        move |_c: Context, _i: Value| {
            let w = w.clone();
            async move { Ok(w) }
        },
        NodeConfig::default(),
    );
    let events = drain(node.run(top_level_ctx(), Value::Null)).await;
    assert_eq!(events.len(), 1, "expected 1 event");
    assert_eq!(events[0].output, None, "Content must not go to Output");
    assert_eq!(events[0].response.content, Some(want));
}

#[tokio::test]
async fn test_function_node_validate_output() {
    let fn_body = |_c: Context, _i: Input| async { Ok(Value::Null) }; // unused: validate_output is exercised directly
    let schema_node = function_node_with_schema::<Input, Value, _, _>(
        "test",
        fn_body,
        Some(infer_schema::<Input>()),
        Some(infer_schema::<TargetOutput>()),
        NodeConfig::default(),
    )
    .expect("NewFunctionNodeWithSchema failed");
    let nil_schema_node =
        function_node::<Input, Value, _, _>("test_nil", fn_body, NodeConfig::default());

    // Case "direct_valid_passes_through".
    assert_eq!(
        schema_node
            .validate_output(json!({"result": 1}))
            .expect("valid output"),
        json!({"result": 1})
    );
    // Case "schema_mismatch_fails".
    assert!(
        schema_node
            .validate_output(json!({"result": "not-an-int"}))
            .is_err(),
        "ValidateOutput: expected error"
    );
    // Case "nil_schema_passes_through".
    assert_eq!(
        nil_schema_node
            .validate_output(json!({"anything": 1}))
            .expect("no schema"),
        json!({"anything": 1})
    );
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct TwoFieldParams {
    #[serde(rename = "foo_key")]
    foo: String,
    #[serde(rename = "Bar")]
    bar: i64,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct NodeInputParams {
    #[serde(rename = "node_input")]
    predecessor_output: String,
    other_val: i64,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "PascalCase")]
struct OutputStruct {
    message: String,
    code: i64,
}

/// Port of Go mockSessionForTest/mockStateForTest: a context whose session state is `state`.
fn ctx_with_state(state: Value) -> Context {
    let ctx = ctx_with_events(Vec::new());
    for (k, v) in state.as_object().cloned().unwrap_or_default() {
        ctx.session().state().set(&k, v).expect("set state");
    }
    ctx
}

async fn run_from_state(
    node: &Arc<dyn Node>,
    state: Value,
    input: Value,
) -> Result<Option<Value>, String> {
    let (events, err) = collect(node.run(ctx_with_state(state), input)).await;
    match err {
        Some(e) => Err(e.to_string()),
        None => Ok(events.last().and_then(|ev| ev.output.clone())),
    }
}

// parity: adapted: Go `state:"key"` tags become `#[serde(rename = "key")]` and the node_input field is the property named `node_input`; Go's "multiple node_input fields" error cannot arise (property names are unique).
#[tokio::test]
async fn test_new_function_node_from_state() {
    let two_fields = || -> Vec<String> { vec!["foo_key".into(), "Bar".into()] };
    let assert_names = |node: &Arc<dyn Node>, want: Vec<String>| {
        assert_eq!(
            node.state_field_names().map(<[String]>::to_vec),
            Some(want),
            "StateFieldNames mismatch"
        );
    };

    // Case "Success".
    let node = function_node_from_state::<TwoFieldParams, String, _, _>(
        "test1",
        |_c: InvocationContext, p: TwoFieldParams| async move { Ok(format!("{}-{}", p.foo, p.bar)) },
        NodeConfig::default(),
    )
    .expect("setupFn failed");
    assert_names(&node, two_fields());
    let got = run_from_state(&node, json!({"foo_key": "hello", "Bar": 42}), Value::Null).await;
    assert_eq!(got, Ok(Some(json!("hello-42"))));

    // Case "Missing_state_key".
    let node = function_node_from_state::<TwoFieldParams, String, _, _>(
        "test2",
        |_c: InvocationContext, _p: TwoFieldParams| async { Ok(String::new()) },
        NodeConfig::default(),
    )
    .expect("setupFn failed");
    assert_names(&node, two_fields());
    let err = run_from_state(&node, json!({"foo_key": "hello"}), Value::Null)
        .await
        .expect_err("missing Bar");
    assert!(
        err.contains("missing state value for required field"),
        "got {err}"
    );

    // Case "Type_mismatch".
    let node = function_node_from_state::<TwoFieldParams, String, _, _>(
        "test3",
        |_c: InvocationContext, _p: TwoFieldParams| async { Ok(String::new()) },
        NodeConfig::default(),
    )
    .expect("setupFn failed");
    assert_names(&node, two_fields());
    let err = run_from_state(
        &node,
        json!({"foo_key": "hello", "Bar": "not-an-int"}),
        Value::Null,
    )
    .await
    .expect_err("type mismatch");
    assert!(
        err.contains("failed to convert state value to field"),
        "got {err}"
    );

    // Case "Node_input_success".
    let node = function_node_from_state::<NodeInputParams, String, _, _>(
        "test4",
        |_c: InvocationContext, p: NodeInputParams| async move {
            Ok(format!(
                "input:{},other:{}",
                p.predecessor_output, p.other_val
            ))
        },
        NodeConfig::default(),
    )
    .expect("setupFn failed");
    assert_names(&node, vec!["other_val".into()]);
    let got = run_from_state(&node, json!({"other_val": 100}), json!("from_pred")).await;
    assert_eq!(got, Ok(Some(json!("input:from_pred,other:100"))));

    // Case "Node_input_type_mismatch".
    let node = function_node_from_state::<NodeInputParams, String, _, _>(
        "test5",
        |_c: InvocationContext, _p: NodeInputParams| async { Ok(String::new()) },
        NodeConfig::default(),
    )
    .expect("setupFn failed");
    assert_names(&node, vec!["other_val".into()]);
    let err = run_from_state(&node, json!({"other_val": 100}), json!(123))
        .await
        .expect_err("node_input mismatch");
    assert!(
        err.contains("invalid input type for node_input"),
        "got {err}"
    );

    // Case "Struct_output_success".
    let node = function_node_from_state::<TwoFieldParams, OutputStruct, _, _>(
        "test6",
        |_c: InvocationContext, p: TwoFieldParams| async move {
            Ok(OutputStruct {
                message: p.foo,
                code: p.bar,
            })
        },
        NodeConfig::default(),
    )
    .expect("setupFn failed");
    assert_names(&node, two_fields());
    let got = run_from_state(&node, json!({"foo_key": "hello", "Bar": 42}), Value::Null).await;
    assert_eq!(got, Ok(Some(json!({"Message": "hello", "Code": 42}))));
}

/// Port of Go newHitlNode used as a successor probe: records that it ran.
fn ran_probe(name: &str) -> (Arc<dyn Node>, Arc<AtomicBool>) {
    let ran = Arc::new(AtomicBool::new(false));
    let r = Arc::clone(&ran);
    let node = TestNode::arc(name, NodeConfig::default(), move |_, _| {
        r.store(true, Ordering::SeqCst);
        futures::stream::empty().boxed()
    });
    (node, ran)
}

/// Port of Go findRequestedInputEvent: the first RequestedInput event and the count of them.
fn find_requested_input_event(events: &[Event]) -> (Option<&Event>, usize) {
    let reqs: Vec<&Event> = events
        .iter()
        .filter(|ev| ev.requested_input.is_some())
        .collect();
    (reqs.first().copied(), reqs.len())
}

/// An emitting body that requests input `req` and then pauses (Go `return nil, ErrNodeInterrupted`).
fn asker(req: RequestInput) -> Arc<dyn Node> {
    emitting_function_node::<Value, Value, _, _>(
        "asker",
        move |ctx: Context, _i: Value, emit: Emit| {
            let req = req.clone();
            async move {
                emit(new_request_input_event(ctx.as_invocation(), req)).await?;
                Err::<Value, _>(WorkflowError::sentinel(WorkflowErrorKind::NodeInterrupted).into())
            }
        },
        NodeConfig::default(),
    )
}

#[tokio::test]
async fn test_emitting_function_node_hitl_pauses_and_forwards_request() {
    let node = asker(RequestInput {
        interrupt_id: "human_review".into(),
        message: "Please approve".into(),
        payload: Some(json!("the draft")),
        ..RequestInput::default()
    });
    let (downstream, downstream_ran) = ran_probe("downstream");
    let w = must_new(vec![edge(&start(), &node), edge(&node, &downstream)]);
    let events = drain(w.run(seeded_mock_ctx())).await;
    assert!(
        !downstream_ran.load(Ordering::SeqCst),
        "downstream node ran; HITL pause must suppress successor scheduling"
    );
    let (req, count) = find_requested_input_event(&events);
    assert_eq!(count, 1, "expected exactly 1 RequestedInput event");
    let req = req
        .and_then(|ev| ev.requested_input.as_ref())
        .expect("request");
    assert_eq!(req.interrupt_id, "human_review");
    assert_eq!(req.message, "Please approve");
    assert_eq!(req.payload, Some(json!("the draft")));
}

#[tokio::test]
async fn test_emitting_function_node_auto_generates_interrupt_id() {
    let node = asker(RequestInput {
        message: "approve?".into(),
        ..RequestInput::default()
    });
    let w = must_new(vec![edge(&start(), &node)]);
    let events = drain(w.run(seeded_mock_ctx())).await;
    let (req, count) = find_requested_input_event(&events);
    assert_eq!(count, 1, "expected 1 RequestedInput event");
    let id = &req
        .and_then(|ev| ev.requested_input.as_ref())
        .expect("request")
        .interrupt_id;
    assert!(
        !id.is_empty(),
        "InterruptID is empty; engine must auto-generate when caller omits it"
    );
}

#[tokio::test]
async fn test_emitting_function_node_multiple_requests_park() {
    let node = emitting_function_node::<Value, Value, _, _>(
        "asker",
        |ctx: Context, _i: Value, emit: Emit| async move {
            for id in ["first", "second"] {
                let req = RequestInput {
                    interrupt_id: id.into(),
                    ..RequestInput::default()
                };
                emit(new_request_input_event(ctx.as_invocation(), req)).await?;
            }
            Err::<Value, _>(WorkflowError::sentinel(WorkflowErrorKind::NodeInterrupted).into())
        },
        NodeConfig::default(),
    );
    let w = must_new(vec![edge(&start(), &node)]);
    let events = drain(w.run(seeded_mock_ctx())).await;
    let got: Vec<&String> = events
        .iter()
        .flat_map(|ev| ev.long_running_tool_ids.iter())
        .collect();
    assert!(
        got.contains(&&"first".to_string()) && got.contains(&&"second".to_string()),
        "long-running interrupts = {got:?}, want both first and second"
    );

    // Both interrupts register on the parked node (Go RunState, driven through the scheduler).
    let mut s = Scheduler::new(top_level_ctx(), Arc::clone(&w.graph), 0);
    s.schedule_node(start(), None, String::new(), String::new());
    while let Some(item) = s.next().await {
        item.expect("scheduler error");
    }
    let ns = &s.state.nodes["asker"];
    assert_eq!(ns.status, NodeStatus::Waiting);
    assert_eq!(
        ns.interrupts,
        vec!["first".to_string(), "second".to_string()]
    );
}

#[tokio::test]
async fn test_emitting_function_node_error_after_request_fails() {
    let node = emitting_function_node::<Value, Value, _, _>(
        "asker",
        |ctx: Context, _i: Value, emit: Emit| async move {
            let req = RequestInput {
                interrupt_id: "ignored".into(),
                ..RequestInput::default()
            };
            emit(new_request_input_event(ctx.as_invocation(), req)).await?;
            Err::<Value, _>(test_err("downstream of request"))
        },
        NodeConfig::default(),
    );
    let w = must_new(vec![edge(&start(), &node)]);
    let err = drain_err(w.run(seeded_mock_ctx())).await;
    assert_eq!(
        find_cause::<TestError>(&err),
        Some(&TestError("downstream of request")),
        "Run error = {err}, failure must take precedence over the recorded request"
    );
}

#[tokio::test]
async fn test_emitting_function_node_emit_progress_before_output() {
    let worker = emitting_function_node::<Value, String, _, _>(
        "worker",
        |ctx: Context, _i: Value, emit: Emit| async move {
            let mut progress = Event::new(ctx.platform(), ctx.invocation_id());
            progress.response.content = Some(Content {
                parts: vec![Part::from_text("tick")],
                ..Content::default()
            });
            emit(progress).await?;
            Ok("done".to_string())
        },
        NodeConfig::default(),
    );
    let (downstream, downstream_ran) = ran_probe("downstream");
    let w = must_new(vec![edge(&start(), &worker), edge(&worker, &downstream)]);
    let events = drain(w.run(seeded_mock_ctx())).await;
    assert!(
        downstream_ran.load(Ordering::SeqCst),
        "downstream did not run; emit without ErrNodeInterrupted must not pause"
    );
    let saw_progress = events.iter().any(|ev| {
        ev.response
            .content
            .as_ref()
            .is_some_and(|c| c.parts.iter().any(|p| p.text == "tick"))
    });
    let saw_done = events.iter().any(|ev| ev.output == Some(json!("done")));
    assert!(
        saw_progress,
        r#"did not observe progress event with Content text "tick""#
    );
    assert!(
        saw_done,
        r#"did not observe terminal event with Output="done""#
    );
}

// parity: adapted: Go's "yield returned false" is dropping the stream after the first event; dropping also drops the body, so the body's emit (kept in a slot) is called again afterwards and must fail with AgentError::Cancelled.
#[rstest::rstest]
#[case::returns_emit_error("returns emit error")]
#[case::ignores_emit_error("ignores emit error")]
#[case::returns_independent_error("returns independent error")]
#[tokio::test]
async fn test_emitting_function_node_stops_after_consumer_exits(#[case] mode: &'static str) {
    let slot: Arc<Mutex<Option<Emit>>> = Arc::default();
    let s = Arc::clone(&slot);
    let worker = emitting_function_node::<Value, String, _, _>(
        "worker",
        move |_c: Context, _i: Value, emit: Emit| {
            let s = Arc::clone(&s);
            async move {
                *s.lock() = Some(Arc::clone(&emit));
                let res = emit(Event::default()).await;
                match mode {
                    "returns emit error" => res.map(|()| "done".to_string()),
                    "ignores emit error" => Ok("done".to_string()),
                    _ => Err(AgentError::other("body failed")),
                }
            }
        },
        NodeConfig::default(),
    );
    let mut stream = worker.run(seeded_mock_ctx().into_context(), Value::Null);
    let mut count = 0;
    if let Some(item) = stream.next().await {
        item.expect("unexpected error");
        count += 1;
    }
    drop(stream); // Go: break out of the range loop
    assert_eq!(count, 1, "consumed {count} events, want 1");
    let emit = slot.lock().take().expect("body stored its emit");
    let res = emit(Event::default()).await;
    assert!(
        matches!(res, Err(AgentError::Cancelled)),
        "emit after consumer exit = {res:?}, want Cancelled"
    );
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
struct NameInput {
    name: String,
}

// parity: adapted: Rust inputs are JSON values, so the typed-input "assertion hit" path is the JSON input `{"name": ..}`; the schema check still runs before conversion.
#[tokio::test]
async fn test_function_node_validates_input_on_assertion_hit_path() {
    let input_schema =
        json!({"type": "object", "properties": {"name": {"type": "string", "maxLength": 3}}});
    async fn run_once(node: &Arc<dyn Node>, name: &str) -> Option<AgentError> {
        collect(node.run(top_level_ctx(), json!({"name": name})))
            .await
            .1
    }

    // Subtest "plain".
    let node = function_node_with_schema::<NameInput, String, _, _>(
        "greet",
        |_c: Context, i: NameInput| async move { Ok(i.name) },
        Some(input_schema.clone()),
        None,
        NodeConfig::default(),
    )
    .expect("NewFunctionNodeWithSchema");
    assert!(
        run_once(&node, "ok").await.is_none(),
        "valid input rejected"
    );
    let err = run_once(&node, "too-long")
        .await
        .expect("over-length input must fail");
    assert!(
        err.to_string().contains("validation failed for input"),
        "over-length input: err = {err}"
    );

    // Subtest "emitting".
    let node = emitting_function_node_with_schema::<NameInput, Value, _, _>(
        "greet",
        |_c: Context, i: NameInput, _e: Emit| async move { Ok(json!(i.name)) },
        Some(input_schema),
        None,
        NodeConfig::default(),
    )
    .expect("NewEmittingFunctionNodeWithSchema");
    assert!(
        run_once(&node, "ok").await.is_none(),
        "valid input rejected"
    );
    let err = run_once(&node, "too-long")
        .await
        .expect("over-length input must fail");
    assert!(
        err.to_string().contains("validation failed for input"),
        "over-length input: err = {err}"
    );
}

#[tokio::test]
async fn test_function_node_non_content_output_goes_to_output() {
    let want = json!({"result": "ok"});
    let w = want.clone();
    let node = function_node::<Value, Value, _, _>(
        "n",
        move |_c: Context, _i: Value| {
            let w = w.clone();
            async move { Ok(w) }
        },
        NodeConfig::default(),
    );
    let events = drain(node.run(top_level_ctx(), Value::Null)).await;
    assert_eq!(
        events.len(),
        1,
        "FunctionNode.Run emitted {} events, want 1",
        events.len()
    );
    assert_eq!(
        events[0].response.content, None,
        "non-Content output must not set Content"
    );
    assert_eq!(events[0].output, Some(want));
}

#[derive(Deserialize, schemars::JsonSchema)]
#[allow(dead_code)]
struct ValidParams {
    #[serde(rename = "Foo")]
    foo: String,
    node_input: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[allow(dead_code)]
struct InvalidParams {
    foo: String,
}

/// Not a Go test of this file: exercises Task 8's state-schema consistency check now that
/// `function_node_from_state` exists (the Go cases of TestWorkflow_StateSchemaConsistency).
#[tokio::test]
async fn state_schema_consistency_with_from_state_nodes() {
    let schema = json!({"type": "object", "properties": {"Foo": {"type": "string"}}});
    let valid = function_node_from_state::<ValidParams, String, _, _>(
        "valid_node",
        |_c: InvocationContext, _p: ValidParams| async { Ok(String::new()) },
        NodeConfig::default(),
    )
    .expect("valid node");
    let invalid = function_node_from_state::<InvalidParams, String, _, _>(
        "invalid_node",
        |_c: InvocationContext, _p: InvalidParams| async { Ok(String::new()) },
        NodeConfig::default(),
    )
    .expect("invalid node");
    let build = |n: &Arc<dyn Node>, schema: Option<Value>| {
        let opts = WorkflowOptions {
            state_schema: schema,
            ..WorkflowOptions::default()
        };
        Workflow::new("wf", vec![edge(&start(), n)], opts).map(|_| ())
    };
    assert!(
        build(&valid, Some(schema.clone())).is_ok(),
        "valid schema matches exactly"
    );
    let err = build(&invalid, Some(schema.clone())).expect_err("typo in tag causes error");
    assert!(
        err.to_string().contains(r#"node "invalid_node" references state field "foo" which is not declared in StateSchema"#),
        "got: {err}"
    );
    assert!(build(&invalid, None).is_ok(), "nil schema skips validation");
    assert!(
        build(&dummy_node("agentNode"), Some(schema)).is_ok(),
        "non state params aware nodes are ignored"
    );
}
