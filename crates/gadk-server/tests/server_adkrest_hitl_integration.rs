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

//! Port of Go `server/adkrest/hitl_integration_test.go`: the human-in-the-loop pause/resume flow
//! the web UI drives over `/run`, two full cycles in one session.

use std::sync::Arc;

use gadk_agent::{Context, SingleLoader};
use gadk_engine::agents::workflow_agent::{self, WorkflowAgentConfig};
use gadk_engine::workflow::{
    Emit, NodeConfig, START, WORKFLOW_INPUT_FUNCTION_CALL_NAME, chain, emitting_function_node,
    function_node, new_request_input_event,
};
use gadk_engine::{WorkflowError, WorkflowErrorKind};
use gadk_genai::{Content, FunctionResponse};
use gadk_server::rest::testutil::{authenticate_as, post_json};
use gadk_server::rest::{Server, ServerConfig};
use gadk_session::{InMemorySessionService, RequestInput};
use serde_json::{Value, json};

const HITL_APP: &str = "hitl_simple";
const HITL_USER: &str = "u";

/// Go `newHITLServer`: a workflow agent shaped like `examples/workflow/hitl_simple` (`ask_name`
/// pauses on a fresh interrupt ID per request, `greet` answers the reply), every caller
/// authenticated as `authenticated_user_id`.
fn new_hitl_server(authenticated_user_id: &'static str) -> axum::Router {
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
            Err::<Value, _>(WorkflowError::sentinel(WorkflowErrorKind::NodeInterrupted).into())
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
    let agent = workflow_agent::new(WorkflowAgentConfig {
        name: HITL_APP.into(),
        edges: chain(&[Arc::clone(&START), ask, greet]),
        ..Default::default()
    })
    .unwrap();
    let mut cfg = ServerConfig::new(Arc::new(InMemorySessionService::new()));
    cfg.agent_loader = Some(Arc::new(SingleLoader::new(agent)));
    cfg.authenticator = Some(authenticate_as(authenticated_user_id));
    Server::new(cfg).unwrap().router()
}

/// Go `createSession`.
async fn create_session(router: &axum::Router) -> String {
    let resp: Value = post_json(
        router,
        &format!("/apps/{HITL_APP}/users/{HITL_USER}/sessions"),
        &json!({}),
    )
    .await;
    let id = resp["id"].as_str().unwrap_or_default().to_owned();
    assert!(!id.is_empty(), "create session returned an empty ID");
    id
}

/// Go `runTurn`: the REST events of one `/run`.
async fn run_turn(router: &axum::Router, sid: &str, msg: &Content) -> Vec<Value> {
    post_json(
        router,
        "/run",
        &json!({
            "appName": HITL_APP,
            "userId": HITL_USER,
            "sessionId": sid,
            "newMessage": msg,
        }),
    )
    .await
}

/// Go `pauseInterruptID`: the first `requestedInput.interruptId`, else long-running tool ID.
fn pause_interrupt_id(events: &[Value]) -> String {
    for ev in events {
        if let Some(id) = ev["requestedInput"]["interruptId"].as_str()
            && !id.is_empty()
        {
            return id.to_owned();
        }
        if let Some(id) = ev["longRunningToolIds"][0].as_str() {
            return id.to_owned();
        }
    }
    String::new()
}

/// Go `greetingOutput`: the first non-empty string `output`.
fn greeting_output(events: &[Value]) -> String {
    events
        .iter()
        .filter_map(|ev| ev["output"].as_str())
        .find(|s| !s.is_empty())
        .unwrap_or_default()
        .to_owned()
}

/// Go `restCycle`: a fresh turn that pauses, then a resume turn delivering `name`.
async fn rest_cycle(router: &axum::Router, sid: &str, name: &str) -> (String, String) {
    let paused = run_turn(router, sid, &Content::from_text("hi", "user")).await;
    let interrupt_id = pause_interrupt_id(&paused);
    assert!(
        !interrupt_id.is_empty(),
        "fresh turn did not pause on an interrupt; events = {paused:?}"
    );
    let reply = Content::from_function_response(
        FunctionResponse {
            id: interrupt_id.clone(),
            name: WORKFLOW_INPUT_FUNCTION_CALL_NAME.into(),
            response: json!({"payload": name}).as_object().cloned().unwrap(),
            ..Default::default()
        },
        "user",
    );
    let resumed = run_turn(router, sid, &reply).await;
    (interrupt_id, greeting_output(&resumed))
}

/// Two HITL cycles in one session: each resumes correctly, and each pause carries a distinct
/// interrupt ID (the web UI keys its reply box on it and will not re-prompt for a reused one).
#[tokio::test]
async fn test_resthitl_two_full_cycles_same_session() {
    let router = new_hitl_server("u");
    let sid = create_session(&router).await;

    let (id1, greet1) = rest_cycle(&router, &sid, "Wojtek").await;
    assert_eq!(greet1, "Hello, Wojtek!", "cycle 1 greeting");

    let (id2, greet2) = rest_cycle(&router, &sid, "Karol").await;
    assert_eq!(greet2, "Hello, Karol!", "cycle 2 greeting");

    assert!(
        !id1.is_empty() && !id2.is_empty(),
        "each pause must carry an interrupt ID; got {id1:?} and {id2:?}"
    );
    assert_ne!(
        id1, id2,
        "interrupt IDs must differ across runs in one session (the Dev UI won't re-prompt for a reused ID)"
    );
}
