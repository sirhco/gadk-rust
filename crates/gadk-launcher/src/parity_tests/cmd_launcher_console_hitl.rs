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

//! Port of ADK-Go `cmd/launcher/console/hitl_test.go`.
//!
//! Go captures `os.Stdout`; the Rust render functions write to the `Write` they are given. Go
//! decodes every JSON number as `float64`; serde keeps integers as integers, so the expected
//! values use `json!` integers where Go writes `float64(n)`.

use std::sync::Arc;

use futures::StreamExt;
use gadk_agent::{AgentError, Context, RunConfig};
use gadk_engine::agents::workflow_agent::{self, WorkflowAgentConfig};
use gadk_engine::runner::{PluginConfig, RunOptions, Runner, RunnerConfig};
use gadk_engine::workflow::{
    Emit, NodeConfig, START, WORKFLOW_INPUT_FUNCTION_CALL_NAME, chain, emitting_function_node,
    function_node, new_request_input_event,
};
use gadk_engine::{WorkflowError, WorkflowErrorKind};
use gadk_genai::{Content, FunctionCall, Part};
use gadk_session::{CreateRequest, Event, InMemorySessionService, RequestInput, SessionService};
use pretty_assertions::assert_eq;
use serde_json::{Map, Value, json};

use crate::console::hitl::{
    PendingInterrupt, build_interrupt_response, collect_pending_interrupts, render_output,
    render_workflow_input_prompt,
};

/// Go `toolconfirmation.FunctionCallName`.
const TOOL_CONFIRMATION: &str = gadk_agent::REQUEST_CONFIRMATION_FUNCTION_CALL_NAME;

fn obj(v: Value) -> Map<String, Value> {
    v.as_object().cloned().unwrap_or_default()
}

/// An event whose content is `parts` (Go `LLMResponse.Content`).
fn event(long_running: &[&str], partial: bool, parts: Vec<Part>) -> Event {
    let mut ev = Event {
        long_running_tool_ids: long_running.iter().map(|s| (*s).to_owned()).collect(),
        ..Default::default()
    };
    ev.response.partial = partial;
    ev.response.content = Some(Content {
        parts,
        ..Default::default()
    });
    ev
}

fn call_part(id: &str, name: &str, args: Value) -> Part {
    Part {
        function_call: Some(FunctionCall {
            id: id.into(),
            name: name.into(),
            args: obj(args),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn text_part(text: &str) -> Part {
    Part {
        text: text.into(),
        ..Default::default()
    }
}

fn pending(id: &str, name: &str, args: Value) -> PendingInterrupt {
    PendingInterrupt {
        id: id.into(),
        name: name.into(),
        args: obj(args),
    }
}

/// Go: an event is a HITL prompt iff it has non-empty long-running tool IDs and one of its content
/// parts is a function call whose ID is in that set; the function name is not the discriminator.
// parity: adapted: Go's nil args are an empty map
#[test]
fn test_collect_pending_interrupts_detection_by_long_running_tool_i_ds() {
    let cases: Vec<(&str, Vec<Event>, Vec<PendingInterrupt>)> = vec![
        ("empty event list", vec![], vec![]),
        (
            "event with FunctionCall but no LongRunningToolIDs is not an interrupt",
            vec![event(
                &[],
                false,
                vec![call_part("x", "regular_tool", Value::Null)],
            )],
            vec![],
        ),
        (
            "event with LongRunningToolIDs but no matching FunctionCall is not an interrupt",
            vec![event(
                &["abc"],
                false,
                vec![call_part("different_id", "unmatched", Value::Null)],
            )],
            vec![],
        ),
        (
            "workflow input on Event.LLMResponse.Content is detected",
            vec![event(
                &["int-1"],
                false,
                vec![call_part(
                    "int-1",
                    WORKFLOW_INPUT_FUNCTION_CALL_NAME,
                    json!({"message": "ok?"}),
                )],
            )],
            vec![pending(
                "int-1",
                WORKFLOW_INPUT_FUNCTION_CALL_NAME,
                json!({"message": "ok?"}),
            )],
        ),
        (
            "tool confirmation on Event.LLMResponse.Content is detected",
            vec![event(
                &["conf-1"],
                false,
                vec![call_part(
                    "conf-1",
                    TOOL_CONFIRMATION,
                    json!({"toolConfirmation": {"hint": "really delete?"}}),
                )],
            )],
            vec![pending(
                "conf-1",
                TOOL_CONFIRMATION,
                json!({"toolConfirmation": {"hint": "really delete?"}}),
            )],
        ),
        (
            "multiple events, only ones with matching IDs surface",
            vec![
                event(&[], false, vec![text_part("intro")]),
                event(
                    &["int-2"],
                    false,
                    vec![call_part("int-2", "x", Value::Null)],
                ),
                event(&[], false, vec![text_part("outro")]),
            ],
            vec![pending("int-2", "x", Value::Null)],
        ),
        (
            // SSE emits the same function-call event as partial chunks and a final aggregated
            // event; the interrupt surfaces exactly once, from the final event.
            "duplicate call ID across partial and final events dedups to one",
            vec![
                event(
                    &["dup-1"],
                    true,
                    vec![call_part("dup-1", "ask", json!({"a": 1.0}))],
                ),
                event(
                    &["dup-1"],
                    false,
                    vec![call_part("dup-1", "ask", json!({"a": 1.0}))],
                ),
            ],
            vec![pending("dup-1", "ask", json!({"a": 1.0}))],
        ),
        (
            "partial-only event does not surface an interrupt",
            vec![event(
                &["p-1"],
                true,
                vec![call_part("p-1", "ask", Value::Null)],
            )],
            vec![],
        ),
    ];
    for (name, events, want) in cases {
        assert_eq!(collect_pending_interrupts(&events), want, "{name}");
    }
}

/// Go: a JSON object reply is returned verbatim; scalars, arrays and raw text are wrapped under
/// `payload`.
// parity: adapted: JSON integers stay integers (Go float64)
#[test]
fn test_build_interrupt_response_workflow_input() {
    let cases = [
        (
            "raw text is wrapped under payload",
            "approve\n",
            json!({"payload": "approve"}),
        ),
        (
            "JSON object is returned verbatim (no wrapper)",
            "{\"approved\": true, \"days\": 3}\n",
            json!({"approved": true, "days": 3}),
        ),
        (
            "JSON scalar is wrapped under payload",
            "42\n",
            json!({"payload": 42}),
        ),
        (
            "JSON array is wrapped under payload",
            "[1, 2, \"three\"]\n",
            json!({"payload": [1, 2, "three"]}),
        ),
        (
            "trailing CR is stripped",
            "approve\r\n",
            json!({"payload": "approve"}),
        ),
    ];
    for (name, input, want) in cases {
        let p = pending("x", WORKFLOW_INPUT_FUNCTION_CALL_NAME, Value::Null);
        let part = build_interrupt_response(&p, input);
        let fr = part.function_response.expect(name);
        assert_eq!(fr.id, "x", "{name}: ID");
        assert_eq!(fr.name, WORKFLOW_INPUT_FUNCTION_CALL_NAME, "{name}: Name");
        assert_eq!(Value::Object(fr.response), want, "{name}: Response");
    }
}

/// Go: positive answers map to `{"confirmed": true}`, everything else (blank lines included) to
/// `{"confirmed": false}`, case-insensitive.
#[test]
fn test_build_interrupt_response_tool_confirmation() {
    let cases = [
        ("yes\n", true),
        ("YES\n", true),
        (" Yes \n", true),
        ("y\n", true),
        ("true\n", true),
        ("confirm\n", true),
        ("no\n", false),
        ("n\n", false),
        ("\n", false),
        ("anything else\n", false),
    ];
    for (input, want) in cases {
        let p = pending("c", TOOL_CONFIRMATION, Value::Null);
        let part = build_interrupt_response(&p, input);
        let response = part.function_response.expect(input).response;
        let Some(confirmed) = response.get("confirmed") else {
            panic!("Response missing 'confirmed'; got {response:?}");
        };
        assert_eq!(confirmed, &Value::Bool(want), "{input:?}");
    }
}

/// Go: the catch-all path for any long-running call name the launcher does not know.
// parity: adapted: JSON integers stay integers (Go float64)
#[test]
fn test_build_interrupt_response_generic_fallback() {
    let cases = [
        (
            "raw text is wrapped under result",
            "some_value\n",
            json!({"result": "some_value"}),
        ),
        (
            "JSON object is returned verbatim (no wrapper)",
            "{\"foo\": \"bar\"}\n",
            json!({"foo": "bar"}),
        ),
        (
            "JSON scalar is wrapped under result",
            "42\n",
            json!({"result": 42}),
        ),
        (
            "JSON array is wrapped under result",
            "[1, 2]\n",
            json!({"result": [1, 2]}),
        ),
    ];
    for (name, input, want) in cases {
        let p = pending("g", "some_unknown_kind", Value::Null);
        let part = build_interrupt_response(&p, input);
        assert_eq!(
            Value::Object(part.function_response.expect(name).response),
            want,
            "{name}"
        );
    }
}

/// Go: a string payload prints raw; an object payload prints as JSON aligned under the
/// `  Payload: ` label (keys at column 4, the closing brace at column 2).
#[test]
fn test_render_workflow_input_prompt_payload_formatting() {
    let cases: [(&str, Value, &[&str], &[&str]); 2] = [
        (
            "string payload prints raw, no escaped quotes",
            json!({"message": "approve?", "payload": "hello world"}),
            &["Agent -> approve?\n", "  Payload: hello world\n"],
            &["\"hello world\"", "\\\""],
        ),
        (
            "object payload is indented under the Payload label",
            json!({"message": "ok?", "payload": {"user": "alice", "days": 5.0}}),
            &[
                "  Payload: {\n",
                "\n    \"days\": 5",
                "\n    \"user\": \"alice\"",
                "\n  }\n",
            ],
            &["\n  \"days\"", "\n}\n"],
        ),
    ];
    for (name, args, want_contains, want_absent) in cases {
        let mut out = Vec::new();
        render_workflow_input_prompt(&mut out, &obj(args));
        let out = String::from_utf8(out).unwrap();
        for s in want_contains {
            assert!(
                out.contains(s),
                "{name}: output missing {s:?}\nfull output:\n{out}"
            );
        }
        for s in want_absent {
            assert!(
                !out.contains(s),
                "{name}: output contains forbidden {s:?}\nfull output:\n{out}"
            );
        }
    }
}

// parity: adapted: Go's Output is `any` (the struct case is a Go struct); gadk's is a JSON value
#[test]
fn test_render_output() {
    let cases = [
        ("string verbatim", json!("Hello, Alice!"), "Hello, Alice!"),
        ("empty string", json!(""), ""),
        ("int", json!(42), "42"),
        (
            "struct as json",
            json!({"result": "ok"}),
            "{\"result\":\"ok\"}",
        ),
    ];
    for (name, input, want) in cases {
        assert_eq!(render_output(&input), want, "{name}");
    }
}

/// Go: drives the console's HITL handling (`collect_pending_interrupts` and
/// `build_interrupt_response`) against a live runner for two pause/resume cycles in one session,
/// as the REPL loop does.
#[tokio::test]
async fn test_console_hitl_two_full_cycles_same_session() {
    const APP: &str = "hitl_simple";
    const USER: &str = "u";
    const SID: &str = "s";

    let svc: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    svc.create(CreateRequest {
        app_name: APP.into(),
        user_id: USER.into(),
        session_id: Some(SID.into()),
        ..Default::default()
    })
    .await
    .expect("session Create()");
    let r = Runner::new(RunnerConfig {
        app_name: APP.into(),
        agent: new_console_hitl_agent(),
        session_service: svc,
        artifact_service: None,
        memory_service: None,
        plugin_config: PluginConfig::default(),
        auto_create_session: false,
        platform: Default::default(),
        compaction: None,
    })
    .expect("runner.New()");

    let turn = |msg: Content| {
        let r = &r;
        async move {
            let mut events = Vec::new();
            let mut final_output = None;
            let mut stream = r.run(
                USER,
                SID,
                Some(msg),
                RunConfig::default(),
                RunOptions::default(),
            );
            while let Some(item) = stream.next().await {
                let ev = item.unwrap_or_else(|e| panic!("Run() error = {e}"));
                if ev.response.content.is_none() && ev.output.is_some() {
                    final_output = ev.output.clone();
                }
                events.push(ev);
            }
            (collect_pending_interrupts(&events), final_output)
        }
    };

    let cycle = |name: &'static str| {
        let turn = &turn;
        async move {
            let (pending, _) = turn(Content::from_text("hi", "user")).await;
            assert_eq!(pending.len(), 1, "fresh turn: pending interrupts");
            let reply = Content::from_parts(
                vec![build_interrupt_response(&pending[0], &format!("{name}\n"))],
                "user",
            );
            turn(reply).await.1
        }
    };

    assert_eq!(
        cycle("Wojtek").await,
        Some(json!("Hello, Wojtek!")),
        "cycle 1 greeting"
    );
    assert_eq!(
        cycle("Karol").await,
        Some(json!("Hello, Karol!")),
        "cycle 2 greeting"
    );
}

/// Go `newConsoleHITLAgent`: a workflow agent shaped like `examples/workflow/hitl_simple`;
/// `ask_name` pauses on a unique interrupt ID per request, `greet` returns the greeting.
fn new_console_hitl_agent() -> Arc<dyn gadk_agent::Agent> {
    let ask = emitting_function_node::<Value, Value, _, _>(
        "ask_name",
        |ctx: Context, _input: Value, emit: Emit| async move {
            let interrupt_id = format!("ask_name-{}", ctx.as_invocation().platform().new_uuid());
            emit(new_request_input_event(
                ctx.as_invocation(),
                RequestInput {
                    interrupt_id,
                    message: "What's your name?".into(),
                    ..Default::default()
                },
            ))
            .await?;
            Err::<Value, AgentError>(
                WorkflowError::sentinel(WorkflowErrorKind::NodeInterrupted).into(),
            )
        },
        NodeConfig::default(),
    );
    let greet = function_node::<String, String, _, _>(
        "greet",
        |_ctx: Context, name: String| async move {
            let name = if name.is_empty() {
                "stranger".to_owned()
            } else {
                name
            };
            Ok(format!("Hello, {name}!"))
        },
        NodeConfig::default(),
    );
    workflow_agent::new(WorkflowAgentConfig {
        name: "hitl_simple".into(),
        edges: chain(&[Arc::clone(&START), ask, greet]),
        ..Default::default()
    })
    .expect("workflowagent.New()")
}
