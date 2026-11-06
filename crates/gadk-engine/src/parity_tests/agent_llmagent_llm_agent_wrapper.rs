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

//! Port of Go agent/llmagent/llm_agent_wrapper_test.go.

use std::any::Any;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use futures::StreamExt;
use futures::stream::BoxStream;
use gadk_agent::{
    Agent, AgentConfig, Context, ContextDelta, InvocationContext, Mode, RunConfig, StreamingMode,
};
use gadk_genai::{Content, FunctionCall, Part, ROLE_MODEL, ROLE_USER, Schema};
use gadk_model::{Llm, LlmRequest, LlmResponse, ModelError};
use gadk_platform::Platform;
use gadk_session::{CreateRequest, Event, InMemorySessionService, NodeInfo, SessionService};
use serde_json::{Map, Value, json};

use crate::llm::agent as llm_agent;
use crate::llm::node_wrapper::{
    prepare_llm_agent_input, process_llm_agent_output, run_llm_agent_as_node,
};
use crate::llm::state::reveal;
use crate::llm::{IncludeContents, LlmAgentConfig};
use crate::runner::{PluginConfig, RunOptions, Runner, RunnerConfig};
use crate::tool::finish_task::FINISH_TASK_SUCCESS_RESULT;

/// Port of Go scriptedLLM: one scripted response per call, then the done text ("done" unless
/// overridden) forever.
struct ScriptedLlm {
    turns: Mutex<VecDeque<LlmResponse>>,
    done_text: &'static str,
}
impl ScriptedLlm {
    fn new(turns: Vec<LlmResponse>) -> Arc<Self> {
        Self::with_done_text(turns, "")
    }
    fn with_done_text(turns: Vec<LlmResponse>, done_text: &'static str) -> Arc<Self> {
        Arc::new(Self {
            turns: Mutex::new(turns.into()),
            done_text,
        })
    }
}
impl Llm for ScriptedLlm {
    fn name(&self) -> &str {
        "scripted-mock"
    }
    fn generate_content(
        &self,
        _req: LlmRequest,
        _stream: bool,
    ) -> BoxStream<'static, Result<LlmResponse, ModelError>> {
        let text = if self.done_text.is_empty() {
            "done"
        } else {
            self.done_text
        };
        let resp = self
            .turns
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| text_resp(text));
        futures::stream::once(async move { Ok(resp) }).boxed()
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Port of Go recordingLLM: keeps the first request, always answers "answer".
#[derive(Default)]
struct RecordingLlm {
    got: Mutex<Option<LlmRequest>>,
}
impl Llm for RecordingLlm {
    fn name(&self) -> &str {
        "recording"
    }
    fn generate_content(
        &self,
        req: LlmRequest,
        _stream: bool,
    ) -> BoxStream<'static, Result<LlmResponse, ModelError>> {
        let mut got = self.got.lock().unwrap();
        if got.is_none() {
            *got = Some(req);
        }
        futures::stream::once(async { Ok(text_resp("answer")) }).boxed()
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn obj(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        other => panic!("not an object: {other}"),
    }
}
fn schema(v: Value) -> Schema {
    serde_json::from_value(v).unwrap()
}
fn text_resp(text: &str) -> LlmResponse {
    LlmResponse {
        content: Some(Content::from_text(text, ROLE_MODEL)),
        ..Default::default()
    }
}
/// Go fcContent, as a response.
fn fc_resp(id: &str, name: &str, args: Value) -> LlmResponse {
    let fc = FunctionCall {
        id: id.into(),
        name: name.into(),
        args: obj(args),
        ..Default::default()
    };
    LlmResponse {
        content: Some(Content::from_function_call(fc, ROLE_MODEL)),
        ..Default::default()
    }
}
fn runner_over(app: &str, agent: Arc<dyn Agent>, svc: Arc<dyn SessionService>) -> Runner {
    Runner::new(RunnerConfig {
        app_name: app.into(),
        agent,
        session_service: svc,
        artifact_service: None,
        memory_service: None,
        plugin_config: PluginConfig::default(),
        auto_create_session: false,
        platform: Platform::default(),
    })
    .unwrap()
}

/// A model-role event with the given parts (Go `&session.Event{LLMResponse: {Content: …}}`).
fn model_event(parts: Vec<Part>) -> Event {
    Event {
        response: LlmResponse {
            content: Some(Content {
                role: ROLE_MODEL.into(),
                parts,
            }),
            ..Default::default()
        },
        ..Default::default()
    }
}

/// Port of Go makeLLMAgent + withMode (a ScriptedLlm with no script).
fn make_llm_agent(name: &str, mode: Mode, f: impl FnOnce(&mut LlmAgentConfig)) -> Arc<dyn Agent> {
    let mut cfg = LlmAgentConfig {
        name: name.into(),
        description: "test agent".into(),
        model: Some(ScriptedLlm::new(vec![])),
        mode,
        ..Default::default()
    };
    f(&mut cfg);
    llm_agent::new(cfg).unwrap()
}

fn plain_agent() -> Arc<dyn Agent> {
    gadk_agent::new_agent(AgentConfig {
        name: "plain".into(),
        ..Default::default()
    })
    .unwrap()
}

/// Port of Go newStubNodeContext.
fn stub_node_ctx(a: &Arc<dyn Agent>, isolation_scope: &str) -> Context {
    let ctx = Context::builder()
        .agent(a.clone())
        .invocation_id("inv-test")
        .build();
    if isolation_scope.is_empty() {
        ctx
    } else {
        ctx.with_delta(ContextDelta {
            isolation_scope: Some(isolation_scope.into()),
            ..Default::default()
        })
    }
}

/// Go `icontext.NewInvocationContext(.., {Agent, Session, UserContent, InvocationID})` over a
/// fresh in-memory session.
async fn session_ctx(
    a: &Arc<dyn Agent>,
    user_text: &str,
    invocation_id: &str,
) -> InvocationContext {
    let svc = InMemorySessionService::new();
    let session = svc
        .create(CreateRequest {
            app_name: "app".into(),
            user_id: "u".into(),
            session_id: None,
            state: None,
        })
        .await
        .unwrap();
    InvocationContext::builder()
        .agent(a.clone())
        .session(session)
        .user_content(Content::from_text(user_text, ROLE_USER))
        .invocation_id(invocation_id)
        .build_invocation()
}

/// Go agent.NewToolContext(ic, "fc-1", ..): the context with a function call id.
fn tool_ctx(ic: InvocationContext) -> Context {
    ic.into_context().with_delta(ContextDelta {
        function_call_id: Some("fc-1".into()),
        ..Default::default()
    })
}

/// Drives `r` for one user turn with SSE streaming, failing on any error.
async fn run_turn(r: &Runner, text: &str) -> Vec<Event> {
    let cfg = RunConfig {
        streaming_mode: StreamingMode::Sse,
        ..Default::default()
    };
    let mut stream = r.run(
        "u",
        "s",
        Some(Content::from_text(text, ROLE_USER)),
        cfg,
        RunOptions::default(),
    );
    let mut events = Vec::new();
    while let Some(ev) = stream.next().await {
        events.push(ev.unwrap_or_else(|e| panic!("runner.Run: {e}")));
    }
    events
}

/// Port of Go runChatCoordinatorOneTurn.
async fn run_chat_coordinator_one_turn(coord: Arc<dyn Agent>, user_text: &str) -> Vec<Event> {
    let svc: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    svc.create(CreateRequest {
        app_name: "app".into(),
        user_id: "u".into(),
        session_id: Some("s".into()),
        state: None,
    })
    .await
    .unwrap_or_else(|e| panic!("session.Create: {e}"));
    let r = runner_over("app", coord, svc);
    run_turn(&r, user_text).await
}

fn parts(ev: &Event) -> impl Iterator<Item = &Part> {
    ev.response.content.iter().flat_map(|c| c.parts.iter())
}

/// Whether `ev` carries a finish_task function call / function response.
fn finish_fc_fr(ev: &Event) -> (bool, bool) {
    let fc = parts(ev).any(|p| {
        p.function_call
            .as_ref()
            .is_some_and(|fc| fc.name == "finish_task")
    });
    let fr = parts(ev).any(|p| {
        p.function_response
            .as_ref()
            .is_some_and(|fr| fr.name == "finish_task")
    });
    (fc, fr)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_dispatch_task_fc_isolation_scope() {
    const COORD: &str = "coordinator";
    const TASK: &str = "doer";
    const FC_ID: &str = "fc-delegation-1";

    // Task sub-agent: a task-mode LlmAgent driven by a scripted LLM that immediately calls
    // finish_task("done") and exits.
    let task_llm = ScriptedLlm::new(vec![fc_resp(
        "finish-1",
        "finish_task",
        json!({"result": "done"}),
    )]);
    let task_agent = llm_agent::new(LlmAgentConfig {
        name: TASK.into(),
        description: "Solves a delegated task.".into(),
        model: Some(task_llm),
        mode: Mode::Task,
        ..Default::default()
    })
    .unwrap_or_else(|e| panic!("llmagent.New(task): {e}"));
    // Coordinator: delegates on its first turn, then replies "delegation complete".
    let coord_llm = ScriptedLlm::new(vec![
        fc_resp(FC_ID, TASK, json!({"request": "do the thing"})),
        text_resp("delegation complete"),
    ]);
    let coord = llm_agent::new(LlmAgentConfig {
        name: COORD.into(),
        model: Some(coord_llm),
        mode: Mode::Chat,
        sub_agents: vec![task_agent],
        ..Default::default()
    })
    .unwrap_or_else(|e| panic!("llmagent.New(coord): {e}"));

    let events = run_chat_coordinator_one_turn(coord, "go").await;

    let task_scopes: Vec<String> = events
        .iter()
        .filter(|e| e.author == TASK)
        .map(|e| e.isolation_scope.clone())
        .collect();
    assert!(
        task_scopes.iter().any(|s| s == FC_ID),
        "expected at least one event authored by task agent {TASK:?} to carry IsolationScope == {FC_ID:?}; observed task-event scopes: {task_scopes:?}"
    );
}

// parity: adapted: node input is a serde_json::Value — Go's *genai.Content case is passed as the Content's JSON (an object with only "parts"/"role", which node_input_to_content recognises; any other object is marshaled text); the nil-ctx subtest is dropped (a Rust Context is not nullable)
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_prepare_llm_agent_input() {
    // nil nodeInput returns nil
    let a = make_llm_agent("x", Mode::SingleTurn, |_| {});
    let ctx = stub_node_ctx(&a, "");
    assert!(prepare_llm_agent_input(a.as_ref(), ctx.as_invocation(), &Value::Null).is_none());

    // nil ctx returns nil instead of panicking: not representable (see the adapted note).

    // non-LlmAgent returns nil
    let plain = plain_agent();
    let ctx = stub_node_ctx(&plain, "");
    assert!(
        prepare_llm_agent_input(plain.as_ref(), ctx.as_invocation(), &json!("ignored")).is_none()
    );

    // chat mode returns nil (no seeding); task mode returns nil (FC-driven, no seeding)
    for (name, mode) in [("chat", Mode::Chat), ("task", Mode::Task)] {
        let a = make_llm_agent(name, mode, |_| {});
        let ctx = stub_node_ctx(&a, "");
        assert!(
            prepare_llm_agent_input(a.as_ref(), ctx.as_invocation(), &json!("input")).is_none(),
            "{name}"
        );
    }

    // single_turn + string input yields user-role text part
    let a = make_llm_agent("st", Mode::SingleTurn, |_| {});
    let ctx = stub_node_ctx(&a, "");
    let got = prepare_llm_agent_input(a.as_ref(), ctx.as_invocation(), &json!("hello there"))
        .expect("llmagent.PrepareLLMAgentInput returned nil; want non-nil event");
    assert_eq!(got.author, "user");
    let content = got.response.content.clone().expect("content");
    assert_eq!(content.role, ROLE_USER);
    assert_eq!(content.parts.len(), 1);
    assert_eq!(content.parts[0].text, "hello there");

    // single_turn + *genai.Content reuses parts and forces role=user
    let input = Content {
        role: "model".into(), // deliberately wrong; must be forced to user
        parts: vec![Part::from_text("part one"), Part::from_text("part two")],
    };
    let got = prepare_llm_agent_input(
        a.as_ref(),
        ctx.as_invocation(),
        &serde_json::to_value(&input).unwrap(),
    )
    .expect("expected non-nil event");
    let content = got.response.content.expect("content");
    assert_eq!(content.role, ROLE_USER, "Role must be forced to user");
    assert_eq!(content.parts, input.parts, "Parts mismatch");

    // single_turn + struct input is JSON-marshaled
    let got = prepare_llm_agent_input(
        a.as_ref(),
        ctx.as_invocation(),
        &json!({"task": "summarize", "limit": 5}),
    )
    .expect("expected non-nil event");
    let ps = got.response.content.expect("content").parts;
    assert_eq!(ps.len(), 1, "Parts count");
    assert!(
        ps[0].text.contains(r#""task":"summarize""#) && ps[0].text.contains(r#""limit":5"#),
        "text {:?} does not look like the marshaled input",
        ps[0].text
    );

    // single_turn + IsolationScope on ctx propagates to event
    let scoped = stub_node_ctx(&a, "iso-scope-xyz");
    let got = prepare_llm_agent_input(a.as_ref(), scoped.as_invocation(), &json!("hello"))
        .expect("expected non-nil event");
    assert_eq!(got.isolation_scope, "iso-scope-xyz");

    // single_turn + empty IsolationScope leaves event scope empty
    let got = prepare_llm_agent_input(a.as_ref(), ctx.as_invocation(), &json!("hello"))
        .expect("expected non-nil event");
    assert_eq!(got.isolation_scope, "");

    // undeclared with no binding returns nil
    let u = make_llm_agent("u", Mode::Unset, |_| {});
    let uctx = stub_node_ctx(&u, "");
    assert!(
        prepare_llm_agent_input(u.as_ref(), uctx.as_invocation(), &json!("hello")).is_none(),
        "PrepareLLMAgentInput for an unplaced undeclared agent, want nil"
    );

    // undeclared bound single_turn by a placement seeds
    let base = Context::builder()
        .agent(u.clone())
        .invocation_id("inv-bound")
        .build();
    let bound = base
        .bound_modes()
        .with(u.name(), u.agent_state().key(), Mode::SingleTurn);
    let bctx = base.with_delta(ContextDelta {
        bound_modes: Some(bound),
        ..Default::default()
    });
    assert!(
        prepare_llm_agent_input(u.as_ref(), bctx.as_invocation(), &json!("hello")).is_some(),
        "expected a seed event for an agent a placement bound single_turn; got nil"
    );
}

// parity: adapted: the "nil event is no-op" subtest is dropped (&mut Event is not nullable)
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_process_llm_agent_output() {
    let x = make_llm_agent("x", Mode::Unset, |_| {});

    // event with FunctionCall is skipped
    let mut ev = model_event(vec![Part {
        function_call: Some(FunctionCall {
            id: "fc-1".into(),
            name: "f".into(),
            ..Default::default()
        }),
        ..Default::default()
    }]);
    process_llm_agent_output(x.as_ref(), &mut ev).unwrap();
    assert_eq!(ev.output, None, "Output on FC event, want nil (skipped)");
    assert!(
        !ev.node_info.as_ref().is_some_and(|n| n.message_as_output),
        "MessageAsOutput = true on FC event, want unset"
    );

    // partial event is skipped
    let mut ev = model_event(vec![Part::from_text("partial")]);
    ev.response.partial = true;
    process_llm_agent_output(x.as_ref(), &mut ev).unwrap();
    assert_eq!(ev.output, None, "Output on partial event, want nil");

    // non-model role is skipped
    let mut ev = model_event(vec![Part::from_text("from user")]);
    ev.response.content.as_mut().unwrap().role = ROLE_USER.into();
    process_llm_agent_output(x.as_ref(), &mut ev).unwrap();
    assert_eq!(ev.output, None, "Output on non-model event, want nil");

    // non-LlmAgent is skipped
    let mut ev = model_event(vec![Part::from_text("hi")]);
    process_llm_agent_output(plain_agent().as_ref(), &mut ev).unwrap();
    assert_eq!(ev.output, None, "Output on non-LlmAgent event, want nil");

    // plain text reply: Output=text, MessageAsOutput=true, no state_delta
    let mut ev = model_event(vec![Part::from_text("the answer")]);
    process_llm_agent_output(x.as_ref(), &mut ev).unwrap();
    assert_eq!(ev.output, Some(json!("the answer")));
    assert!(
        ev.node_info.as_ref().is_some_and(|n| n.message_as_output),
        "MessageAsOutput not set; got NodeInfo = {:?}",
        ev.node_info
    );
    assert!(
        ev.actions.state_delta.as_ref().is_none_or(Map::is_empty),
        "StateDelta = {:?}, want empty (no OutputKey configured)",
        ev.actions.state_delta
    );

    // multi-part text concatenation skips Thought parts
    let mut thought = Part::from_text("thinking out loud");
    thought.thought = true;
    let mut ev = model_event(vec![
        thought,
        Part::from_text("real answer "),
        Part::from_text("continued"),
    ]);
    process_llm_agent_output(x.as_ref(), &mut ev).unwrap();
    assert_eq!(
        ev.output,
        Some(json!("real answer continued")),
        "thought parts must be excluded"
    );

    // OutputKey populates Actions.StateDelta
    let keyed = make_llm_agent("x", Mode::Unset, |c| c.output_key = "my_key".into());
    let mut ev = model_event(vec![Part::from_text("value")]);
    process_llm_agent_output(keyed.as_ref(), &mut ev).unwrap();
    assert_eq!(
        ev.actions.state_delta.unwrap_or_default().get("my_key"),
        Some(&json!("value")),
        "StateDelta[\"my_key\"]"
    );

    // OutputSchema valid JSON: Output is parsed
    let a = make_llm_agent("x", Mode::Unset, |c| {
        c.output_schema = Some(schema(
            json!({"type": "OBJECT", "properties": {"name": {"type": "STRING"}}}),
        ))
    });
    let mut ev = model_event(vec![Part::from_text(r#"{"name":"alice"}"#)]);
    process_llm_agent_output(a.as_ref(), &mut ev).unwrap_or_else(|e| panic!("err = {e}, want nil"));
    let got = match &ev.output {
        Some(Value::Object(m)) => m,
        other => panic!("Output = {other:?}, want a parsed object"),
    };
    assert_eq!(got.get("name"), Some(&json!("alice")));

    // OutputSchema invalid JSON: returns validation error
    let a = make_llm_agent("x", Mode::Unset, |c| {
        c.output_schema = Some(schema(json!({
            "type": "OBJECT",
            "properties": {"age": {"type": "INTEGER"}},
            "required": ["age"]
        })))
    });
    let mut ev = model_event(vec![Part::from_text(r#"{"wrong":"shape"}"#)]);
    assert!(
        process_llm_agent_output(a.as_ref(), &mut ev).is_err(),
        "err = nil, want a schema-validation error"
    );

    // OutputSchema + whitespace-only text: Output stays nil, no error
    let a = make_llm_agent("x", Mode::Unset, |c| {
        c.output_schema = Some(schema(json!({"type": "OBJECT"})))
    });
    let mut ev = model_event(vec![Part::from_text("   ")]);
    process_llm_agent_output(a.as_ref(), &mut ev)
        .unwrap_or_else(|e| panic!("err = {e}, want nil (whitespace short-circuits to nil)"));
    assert_eq!(
        ev.output, None,
        "whitespace text must NOT be returned as the output"
    );
    assert!(
        ev.node_info.as_ref().is_some_and(|n| n.message_as_output),
        "MessageAsOutput should still be set"
    );

    // pre-existing NodeInfo is preserved
    let mut ev = model_event(vec![Part::from_text("ok")]);
    ev.node_info = Some(NodeInfo {
        path: "outer/inner".into(),
        ..Default::default()
    });
    process_llm_agent_output(x.as_ref(), &mut ev).unwrap();
    let ni = ev.node_info.expect("node info");
    assert_eq!(ni.path, "outer/inner", "NodeInfo.Path must be preserved");
    assert!(
        ni.message_as_output,
        "MessageAsOutput not set after preserving Path"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_run_llm_agent_as_node_non_llm_agent_errors() {
    let a = plain_agent();
    let ctx = stub_node_ctx(&a, "");
    let mut got_err = None;
    let mut got_events = 0;
    let mut stream = run_llm_agent_as_node(ctx, a, Value::Null);
    while let Some(item) = stream.next().await {
        match item {
            Ok(_) => got_events += 1,
            Err(e) => got_err = Some(e),
        }
    }
    let err = got_err.expect("expected an error for non-LlmAgent; got nil");
    assert!(
        err.to_string().contains("not an LlmAgent"),
        "err = {err}, want it to mention 'not an LlmAgent'"
    );
    assert_eq!(got_events, 0, "want 0 events (only an error)");
}

// parity: skipped TestRunLLMAgentAsNode_UnsupportedMode_Errors: Go-only — Go Mode is a string type that admits "bogus"; Rust Mode is a closed enum, so an unsupported mode is unrepresentable
// parity: skipped TestRunLLMAgentAsNode_NilContext_Errors: Go nil-interface check — a Rust Context cannot be null

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_run_llm_agent_as_node_declared_single_turn_binds_for_the_request_processors() {
    let llm = Arc::new(RecordingLlm::default());
    let model: Arc<dyn Llm> = llm.clone();
    let a = llm_agent::new(LlmAgentConfig {
        name: "worker".into(),
        model: Some(model),
        mode: Mode::SingleTurn,
        ..Default::default()
    })
    .unwrap_or_else(|e| panic!("llmagent.New: {e}"));

    let svc = InMemorySessionService::new();
    let session = svc
        .create(CreateRequest {
            app_name: "app".into(),
            user_id: "u".into(),
            session_id: None,
            state: None,
        })
        .await
        .unwrap_or_else(|e| panic!("session.Create: {e}"));
    // The current user turn has to be in the session, as the runner puts it there before
    // running the agent.
    for (author, text, role) in [
        ("user", "EARLIER_TURN", ROLE_USER),
        ("worker", "earlier answer", ROLE_MODEL),
        ("user", "current question", ROLE_USER),
    ] {
        let mut ev = Event {
            author: author.into(),
            ..Default::default()
        };
        ev.response.content = Some(Content::from_text(text, role));
        svc.append_event(session.as_ref(), &mut ev)
            .await
            .unwrap_or_else(|e| panic!("AppendEvent: {e}"));
    }

    let ctx = InvocationContext::builder()
        .agent(a.clone())
        .session(session)
        .user_content(Content::from_text("current question", ROLE_USER))
        .invocation_id("inv-declared-single-turn")
        .build_invocation()
        .into_context();
    let mut stream = run_llm_agent_as_node(ctx, a, Value::Null);
    while let Some(item) = stream.next().await {
        item.unwrap_or_else(|e| panic!("RunLLMAgentAsNode: {e}"));
    }

    let got = llm
        .got
        .lock()
        .unwrap()
        .clone()
        .expect("model was never called");
    for c in &got.contents {
        for p in &c.parts {
            assert!(
                !p.text.contains("EARLIER_TURN"),
                "declared single_turn agent saw conversation history; contents = {:?}",
                got.contents
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_run_llm_agent_as_node_task_happy_path() {
    const COORD: &str = "coord";
    const TASK: &str = "doer";
    const FC_ID: &str = "fc-happy-task";
    const EXPECTED_TASK: &str = "summarize";

    let task_llm = ScriptedLlm::new(vec![fc_resp(
        "finish-1",
        "finish_task",
        json!({"result": EXPECTED_TASK}),
    )]);
    let task_agent = make_llm_agent(TASK, Mode::Task, |c| c.model = Some(task_llm));
    let coord_llm = ScriptedLlm::new(vec![
        fc_resp(FC_ID, TASK, json!({"request": "summarize the corpus"})),
        text_resp("delegation complete"),
    ]);
    let coord = llm_agent::new(LlmAgentConfig {
        name: COORD.into(),
        model: Some(coord_llm),
        mode: Mode::Chat,
        sub_agents: vec![task_agent],
        ..Default::default()
    })
    .unwrap_or_else(|e| panic!("llmagent.New(coord): {e}"));

    let events = run_chat_coordinator_one_turn(coord, "go").await;
    let (mut saw_finish_fc, mut saw_success_fr) = (false, false);
    for ev in events.iter().filter(|e| e.author == TASK) {
        let (fc, fr) = finish_fc_fr(ev);
        saw_finish_fc |= fc;
        saw_success_fr |= fr;
    }
    assert!(
        saw_finish_fc,
        "expected the task agent to emit a finish_task FunctionCall event"
    );
    assert!(
        saw_success_fr,
        "expected the task agent to receive a finish_task FunctionResponse event"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_chat_root_two_task_sub_agents_sequential() {
    const COORD: &str = "coord";
    const NAME_A: &str = "sub_a";
    const NAME_B: &str = "sub_b";
    const FC_ID_A: &str = "fc-a";
    const FC_ID_B: &str = "fc-b";

    let make_task_agent = |name: &str| {
        let llm = ScriptedLlm::new(vec![fc_resp(
            &format!("finish-{name}"),
            "finish_task",
            json!({ "result": format!("done-{name}") }),
        )]);
        make_llm_agent(name, Mode::Task, |c| c.model = Some(llm))
    };
    let sub_a = make_task_agent(NAME_A);
    let sub_b = make_task_agent(NAME_B);

    // Coordinator emits FC-A first, then FC-B (after seeing FR-A synthesized into session),
    // then a final "all done" reply.
    let coord_llm = ScriptedLlm::new(vec![
        fc_resp(FC_ID_A, NAME_A, json!({"q": "first"})),
        fc_resp(FC_ID_B, NAME_B, json!({"q": "second"})),
        text_resp("all done"),
    ]);
    let coord = llm_agent::new(LlmAgentConfig {
        name: COORD.into(),
        model: Some(coord_llm),
        mode: Mode::Chat,
        sub_agents: vec![sub_a, sub_b],
        ..Default::default()
    })
    .unwrap();

    let events = run_chat_coordinator_one_turn(coord, "go").await;

    let mut saw_finish_fc: HashMap<String, bool> = HashMap::new();
    let mut saw_success_fr: HashMap<String, bool> = HashMap::new();
    for ev in events
        .iter()
        .filter(|e| e.author == NAME_A || e.author == NAME_B)
    {
        let (fc, fr) = finish_fc_fr(ev);
        if fc {
            saw_finish_fc.insert(ev.author.clone(), true);
        }
        if fr {
            saw_success_fr.insert(ev.author.clone(), true);
        }
    }
    for name in [NAME_A, NAME_B] {
        assert!(
            saw_finish_fc.contains_key(name),
            "task agent {name:?} did not emit finish_task FC"
        );
        assert!(
            saw_success_fr.contains_key(name),
            "task agent {name:?} did not receive finish_task FR"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_chat_task_validation_error_drives_retry() {
    const COORD: &str = "coord";
    const TASK: &str = "doer";
    const FC_ID: &str = "fc-retry-task";
    let output_schema = schema(json!({
        "type": "OBJECT",
        "properties": {"answer": {"type": "STRING"}},
        "required": ["answer"]
    }));

    // Task LLM: a finish_task with the wrong field, then a corrected one.
    let task_llm = ScriptedLlm::with_done_text(
        vec![
            fc_resp("ft-bad", "finish_task", json!({"wrong_field": "oops"})),
            fc_resp("ft-good", "finish_task", json!({"answer": "42"})),
        ],
        "should not be reached",
    );
    let task_agent = make_llm_agent(TASK, Mode::Task, |c| {
        c.output_schema = Some(output_schema);
        c.model = Some(task_llm);
    });
    let coord_llm = ScriptedLlm::new(vec![
        fc_resp(FC_ID, TASK, json!({"request": "what is x?"})),
        text_resp("delegation complete"),
    ]);
    let coord = llm_agent::new(LlmAgentConfig {
        name: COORD.into(),
        model: Some(coord_llm),
        mode: Mode::Chat,
        sub_agents: vec![task_agent],
        ..Default::default()
    })
    .unwrap();

    let events = run_chat_coordinator_one_turn(coord, "go").await;

    let mut finish_fcs = 0;
    let (mut saw_error_fr, mut saw_success_fr) = (false, false);
    for ev in events.iter().filter(|e| e.author == TASK) {
        for p in parts(ev) {
            if p.function_call
                .as_ref()
                .is_some_and(|fc| fc.name == "finish_task")
            {
                finish_fcs += 1;
            }
            if let Some(fr) = p
                .function_response
                .as_ref()
                .filter(|fr| fr.name == "finish_task")
            {
                saw_error_fr |= fr.response.contains_key("error");
                saw_success_fr |= fr.response.get("result").and_then(Value::as_str)
                    == Some(FINISH_TASK_SUCCESS_RESULT);
            }
        }
    }
    assert_eq!(finish_fcs, 2, "expected 2 finish_task FCs (bad then good)");
    assert!(
        saw_error_fr,
        "expected the FinishTaskTool to return an error FR for the bad finish_task args"
    );
    assert!(
        saw_success_fr,
        "expected the FinishTaskTool to return a success FR for the corrected finish_task args"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_chat_coordinator_resumes_unresolved_task_fc() {
    const COORD: &str = "coord";
    const TASK: &str = "doer";
    const FC_ID: &str = "fc-resume-1";

    // Task LLM: when finally invoked (on the resume turn's pre-LLM scan), finishes at once.
    let task_llm = ScriptedLlm::new(vec![fc_resp(
        "finish-1",
        "finish_task",
        json!({"result": "answered"}),
    )]);
    let task_agent = make_llm_agent(TASK, Mode::Task, |c| c.model = Some(task_llm));
    // Coordinator LLM: the pending FC injected below must be re-dispatched BEFORE this call.
    let coord_llm = ScriptedLlm::new(vec![text_resp("post-resume reply")]);
    let coord = llm_agent::new(LlmAgentConfig {
        name: COORD.into(),
        model: Some(coord_llm),
        mode: Mode::Chat,
        sub_agents: vec![task_agent],
        ..Default::default()
    })
    .unwrap();

    // Pre-seed the session with an unresolved task FC authored by the coordinator.
    let svc: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    let session = svc
        .create(CreateRequest {
            app_name: "app".into(),
            user_id: "u".into(),
            session_id: Some("s".into()),
            state: None,
        })
        .await
        .unwrap();
    let mut pending = Event::new(&Platform::default(), "inv-prior");
    pending.author = COORD.into();
    pending.response = fc_resp(FC_ID, TASK, json!({"q": "leftover"}));
    svc.append_event(session.as_ref(), &mut pending)
        .await
        .unwrap_or_else(|e| panic!("AppendEvent(pending FC): {e}"));

    let r = runner_over("app", coord, svc);
    let events = run_turn(&r, "now resume").await;

    let (mut saw_task_finish_fc, mut saw_task_success_fr) = (false, false);
    for ev in events.iter().filter(|e| e.author == TASK) {
        let (fc, fr) = finish_fc_fr(ev);
        saw_task_finish_fc |= fc;
        saw_task_success_fr |= fr;
    }
    assert!(
        saw_task_finish_fc,
        "expected the task agent's finish_task FC to appear during the resume turn"
    );
    assert!(
        saw_task_success_fr,
        "expected the task agent's finish_task success FR to appear during the resume turn"
    );
}

// parity: adapted: Go's tool-context wrapper (agent.NewToolContext) is a Rust Context with function_call_id set via ContextDelta
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_run_llm_agent_as_node_single_turn_accepts_a_tool_context() {
    let llm = Arc::new(RecordingLlm::default());
    let model: Arc<dyn Llm> = llm.clone();
    let a = make_llm_agent("worker", Mode::SingleTurn, |c| c.model = Some(model));

    let ic = session_ctx(&a, "hi", "inv-tool-ctx").await;
    let mut stream = run_llm_agent_as_node(tool_ctx(ic), a, Value::Null);
    while let Some(item) = stream.next().await {
        item.unwrap_or_else(|e| panic!("RunLLMAgentAsNode: {e}"));
    }
    // Only reaching the model rules out a branch that quietly stopped running the agent.
    assert!(
        llm.got.lock().unwrap().is_some(),
        "the model was never called; the single_turn branch did not run the agent"
    );
}

// parity: adapted: Rust contexts are unified — a Context with a function_call_id supports with_delta like any other, so Go's "tool or callback context" error branch does not exist; the port asserts the undeclared (chat) run over that context neither panics nor errors and reaches the model
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_run_llm_agent_as_node_undeclared_over_a_tool_context_errors_rather_than_panics() {
    let a = make_llm_agent("worker", Mode::Unset, |_| {}); // undeclared: resolves to chat

    let ic = session_ctx(&a, "hi", "inv-undeclared-tool-ctx").await;
    let mut stream = run_llm_agent_as_node(tool_ctx(ic), a, Value::Null);
    let mut texts = Vec::new();
    while let Some(item) = stream.next().await {
        let ev = item.unwrap_or_else(|e| panic!("chat run over a tool context: {e}"));
        texts.extend(parts(&ev).map(|p| p.text.clone()));
    }
    assert!(
        texts.iter().any(|t| t == "done"),
        "the model was never called; got texts {texts:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_run_llm_agent_as_node_does_not_mutate_the_agents_include_contents() {
    let llm = Arc::new(RecordingLlm::default());
    let model: Arc<dyn Llm> = llm.clone();
    let a = make_llm_agent("worker", Mode::SingleTurn, |c| c.model = Some(model));
    assert_eq!(
        reveal(a.as_ref())
            .expect("an LlmAgent")
            .cfg
            .include_contents,
        IncludeContents::Unset,
        "precondition: IncludeContents must start unset"
    );

    let ic = session_ctx(&a, "hi", "inv-ic").await;
    let mut stream = run_llm_agent_as_node(ic.into_context(), a.clone(), json!("go"));
    while let Some(item) = stream.next().await {
        item.unwrap_or_else(|e| panic!("RunLLMAgentAsNode: {e}"));
    }
    assert!(
        llm.got.lock().unwrap().is_some(),
        "the model was never called, so the single_turn path was not exercised"
    );

    assert_eq!(
        reveal(a.as_ref())
            .expect("an LlmAgent")
            .cfg
            .include_contents,
        IncludeContents::Unset,
        "IncludeContents after a single_turn run must stay unset (a run must not mutate the agent)"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_llm_agent_new_does_not_mutate_a_sub_agents_mode() {
    let sub = llm_agent::new(LlmAgentConfig {
        name: "worker".into(),
        ..Default::default()
    })
    .unwrap_or_else(|e| panic!("llmagent.New(worker): {e}"));
    assert_eq!(
        reveal(sub.as_ref()).expect("an LlmAgent").cfg.mode,
        Mode::Unset,
        "precondition: declared mode"
    );

    // A declared task sibling, so install_task_tools has something to install and the loop
    // that used to carry the write actually runs over the undeclared one.
    let task = llm_agent::new(LlmAgentConfig {
        name: "tasker".into(),
        mode: Mode::Task,
        ..Default::default()
    })
    .unwrap_or_else(|e| panic!("llmagent.New(tasker): {e}"));
    let coord = llm_agent::new(LlmAgentConfig {
        name: "coordinator".into(),
        mode: Mode::Chat,
        sub_agents: vec![sub.clone(), task],
        ..Default::default()
    })
    .unwrap_or_else(|e| panic!("llmagent.New(coordinator): {e}"));
    assert!(
        !reveal(coord.as_ref())
            .expect("an LlmAgent")
            .tools()
            .is_empty(),
        "the coordinator installed no tools, so install_task_tools did not run"
    );

    assert_eq!(
        reveal(sub.as_ref()).expect("an LlmAgent").cfg.mode,
        Mode::Unset,
        "the undeclared sub-agent's mode after building a coordinator must stay unset"
    );
}
