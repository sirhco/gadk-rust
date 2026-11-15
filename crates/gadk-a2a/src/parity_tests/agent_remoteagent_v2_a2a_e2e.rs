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

//! Ports of ADK-Go `agent/remoteagent/v2/a2a_e2e_test.go`: an a2a-rs client → a2a-rs JSON-RPC
//! server → [`crate::server::Executor`] → gadk agents, some of them remote agents
//! ([`crate::remoteagent::new_a2a`]) calling a second server. Go's helpers (`startA2AServer`,
//! `mockA2AExecutor`, `newA2ARemoteAgent`) are the shared remote-agent harness. The Gemini
//! cases replay ADK-Go's own httprr recordings.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use futures::StreamExt as _;
use gadk_agent::{
    AfterModelCallback, Agent, AgentConfig, AgentError, Context, RunConfig, StreamingMode,
    new_agent,
};
use gadk_engine::llm::config::LlmAgentConfig;
use gadk_engine::tool::{FunctionTool, FunctionToolConfig};
use gadk_genai::{Content, FunctionCall, FunctionResponse, Part, ROLE_MODEL};
use gadk_model::{Llm, LlmRequest, LlmResponse, ModelError};
use gadk_session::{Event, GetRequest, InMemorySessionService, SessionService};
use parking_lot::Mutex;
use serde_json::{Map, Value, json};

use super::agent_remoteagent_v2_a2a_agent::harness::{
    A2AStream, MockExecutor, TestA2AServer, jsonrpc_card, new_a2a_remote_agent, start_a2a_server,
    status_update, submitted_task,
};
use super::server_adka2a_v2_executor::session_service;
use crate::convert::get_a2a_task_info;
use crate::convert::{
    ADK_EXTENSION_URI, is_partial, to_a2a_meta_key, to_a2a_parts, to_genai_parts,
    without_partial_artifacts,
};
use crate::server::{Executor, ExecutorConfig, OutputMode};

const APPROVAL_TOOL_NAME: &str = "request_approval";
const MODEL_TEXT_REQUIRES_APPROVAL: &str = "need to request approval first!";
const MODEL_TEXT_WAITING_FOR_APPROVAL: &str = "waiting for user's approval...";
const MODEL_TEXT_TASK_COMPLETE: &str = "Task complete!";

const TRANSFER_TOOL_NAME: &str = "transfer_to_agent";
const MODEL_TEXT_ROOT_TRANSFER: &str = "transferring... please hold... beepboop...";

const APPROVAL_STATUS_PENDING: &str = "pending";
const APPROVAL_STATUS_APPROVED: &str = "approved";
const APPROVAL_STATUS_VERIFIED: &str = "verified";

/// Go `approval` (Go's `json.Unmarshal` leaves missing fields at their zero value).
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct Approval {
    status: String,
    ticket_id: String,
}

impl Approval {
    /// The tool result (the tools return a JSON object, so no schema derive is needed).
    fn into_map(self) -> Map<String, Value> {
        obj(serde_json::to_value(self).unwrap_or_default())
    }
}

/// The a2a-rs client the tests drive (Go `*a2aclient.Client`).
type Client = a2a_client::A2AClient<Box<dyn a2a_client::Transport>>;

/// Go `newA2AClient`: a JSON-RPC client for `server`.
async fn new_a2a_client(server: &TestA2AServer) -> Client {
    a2a_client::A2AClientFactory::builder()
        .register(Arc::new(a2a_client::jsonrpc::JsonRpcTransportFactory::new(
            None,
        )))
        .build()
        .create_from_card(&jsonrpc_card(&server.url))
        .await
        .unwrap()
}

/// Go `mustSendMessage`.
async fn must_send_message(client: &Client, msg: &a2a::Message) -> a2a::Task {
    let req = a2a::SendMessageRequest {
        message: msg.clone(),
        configuration: None,
        metadata: None,
        tenant: None,
    };
    match client.send_message(&req).await {
        Ok(a2a::SendMessageResponse::Task(task)) => task,
        other => panic!("client.SendMessage() = {other:?}, want a task"),
    }
}

/// Go `a2a.NewMessage(role, parts...)`.
fn new_message(role: a2a::Role, parts: Vec<a2a::Part>) -> a2a::Message {
    a2a::Message::new(role, parts)
}

/// Go `a2a.NewMessageForTask(role, task, parts...)`.
fn new_message_for_task(role: a2a::Role, task: &a2a::Task, parts: Vec<a2a::Part>) -> a2a::Message {
    let mut msg = a2a::Message::new(role, parts);
    msg.task_id = Some(task.id.clone());
    msg.context_id = Some(task.context_id.clone());
    msg
}

/// A text part (Go `a2a.NewTextPart`).
fn text_part(text: &str) -> a2a::Part {
    a2a::Part::text(text)
}

/// A data part with metadata (Go `a2a.NewDataPart` + `SetMeta`).
fn data_part(data: Value, meta: &[(&str, Value)]) -> a2a::Part {
    let mut p = a2a::Part::data(data);
    if !meta.is_empty() {
        p.metadata = Some(
            meta.iter()
                .map(|(k, v)| ((*k).to_owned(), v.clone()))
                .collect(),
        );
    }
    p
}

fn part_text(p: &a2a::Part) -> &str {
    p.as_text().unwrap_or_default()
}

fn obj(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        _ => Map::new(),
    }
}

/// A stub model's answer to one request.
type Reply = Box<dyn Fn(&LlmRequest) -> Vec<Result<LlmResponse, ModelError>> + Send + Sync>;

/// Go `llmStub`: a model answering with `reply(req, stream)`.
struct LlmStub {
    name: String,
    reply: Reply,
}

impl Llm for LlmStub {
    fn name(&self) -> &str {
        &self.name
    }

    fn generate_content(
        &self,
        req: LlmRequest,
        _stream: bool,
    ) -> futures::stream::BoxStream<'static, Result<LlmResponse, ModelError>> {
        futures::stream::iter((self.reply)(&req)).boxed()
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// Go `utils.FunctionResponses(content)`.
fn function_responses(content: Option<&Content>) -> Vec<&FunctionResponse> {
    content
        .map(|c| {
            c.parts
                .iter()
                .filter_map(|p| p.function_response.as_ref())
                .collect()
        })
        .unwrap_or_default()
}

/// Go `newLongRunningTool`.
fn new_long_running_tool() -> Arc<dyn gadk_agent::Tool> {
    FunctionTool::new(
        FunctionToolConfig {
            name: APPROVAL_TOOL_NAME.into(),
            description: "Request approval before proceeding.".into(),
            is_long_running: true,
            ..Default::default()
        },
        |_ctx: Context, _x: Map<String, Value>| async move {
            Ok::<_, gadk_agent::ToolError>(
                Approval {
                    status: APPROVAL_STATUS_PENDING.into(),
                    ticket_id: a2a::new_context_id(),
                }
                .into_map(),
            )
        },
    )
    .unwrap()
}

/// Go `newToolConfirmation`.
fn new_tool_confirmation() -> Arc<dyn gadk_agent::Tool> {
    FunctionTool::new(
        FunctionToolConfig {
            name: APPROVAL_TOOL_NAME.into(),
            description: "Request approval before proceeding.".into(),
            ..Default::default()
        },
        |ctx: Context, _x: Map<String, Value>| async move {
            let Some(confirmation) = ctx.tool_confirmation() else {
                let ticket_id = a2a::new_context_id();
                ctx.request_confirmation("I need approval", Some(json!({"ticket_id": ticket_id})))?;
                return Ok(Approval {
                    status: APPROVAL_STATUS_PENDING.into(),
                    ticket_id,
                }
                .into_map());
            };
            if !confirmation.confirmed {
                return Err(gadk_agent::ToolError::Execution(
                    "confirmation was rejected".into(),
                ));
            }
            let payload: Approval =
                serde_json::from_value(confirmation.payload.clone().unwrap_or_default())
                    .map_err(|e| gadk_agent::ToolError::Execution(Box::new(e)))?;
            Ok(Approval {
                status: APPROVAL_STATUS_VERIFIED.into(),
                ticket_id: payload.ticket_id,
            }
            .into_map())
        },
    )
    .unwrap()
}

/// Go `newInputRequestingAgent`.
fn new_input_requesting_agent(
    name: &str,
    request_approval: Arc<dyn gadk_agent::Tool>,
) -> Arc<dyn Agent> {
    let model = LlmStub {
        name: String::new(),
        reply: Box::new(|req: &LlmRequest| {
            let last = req.contents.last();
            let approval_result = function_responses(last);
            let content = match approval_result.as_slice() {
                // The first model invocation: invoke a long-running tool.
                [] => Content::from_parts(
                    vec![
                        Part::from_text(MODEL_TEXT_REQUIRES_APPROVAL),
                        Part::from_function_call(APPROVAL_TOOL_NAME, Map::new()),
                    ],
                    ROLE_MODEL,
                ),
                // The tool returned a pending result.
                [r] if r.response.get("status") == Some(&json!(APPROVAL_STATUS_PENDING)) => {
                    Content::from_text(MODEL_TEXT_WAITING_FOR_APPROVAL, ROLE_MODEL)
                }
                // User approval is in the session.
                _ => Content::from_text(MODEL_TEXT_TASK_COMPLETE, ROLE_MODEL),
            };
            vec![Ok(LlmResponse {
                content: Some(content),
                ..Default::default()
            })]
        }),
    };
    gadk_engine::llm_agent::new(LlmAgentConfig {
        name: name.into(),
        tools: vec![request_approval],
        model: Some(Arc::new(model)),
        ..Default::default()
    })
    .unwrap()
}

/// Go `newRootAgent`.
fn new_root_agent(name: &str, sub_agent: Arc<dyn Agent>) -> Arc<dyn Agent> {
    let sub_name = sub_agent.name().to_owned();
    let model = LlmStub {
        name: format!("{name}-model"),
        reply: Box::new(move |_req: &LlmRequest| {
            vec![Ok(LlmResponse {
                content: Some(Content::from_parts(
                    vec![
                        Part::from_text(MODEL_TEXT_ROOT_TRANSFER),
                        Part::from_function_call(
                            TRANSFER_TOOL_NAME,
                            obj(json!({"agent_name": sub_name})),
                        ),
                    ],
                    ROLE_MODEL,
                )),
                ..Default::default()
            })]
        }),
    };
    gadk_engine::llm_agent::new(LlmAgentConfig {
        name: name.into(),
        sub_agents: vec![sub_agent],
        model: Some(Arc::new(model)),
        ..Default::default()
    })
    .unwrap()
}

/// gadk-engine's runner config for `agent` (Go `runner.Config{AppName, SessionService, Agent}`).
fn runner_config(
    agent: Arc<dyn Agent>,
    session_service: Arc<dyn SessionService>,
) -> gadk_engine::runner::RunnerConfig {
    gadk_engine::runner::RunnerConfig {
        app_name: agent.name().to_owned(),
        agent,
        session_service,
        artifact_service: None,
        memory_service: None,
        plugin_config: gadk_engine::runner::PluginConfig::default(),
        auto_create_session: false,
        platform: gadk_platform::Platform::default(),
        compaction: None,
    }
}

/// Go `newAgentExecutor`.
fn new_agent_executor(
    agent: Arc<dyn Agent>,
    service: Option<Arc<dyn SessionService>>,
    mode: OutputMode,
) -> Executor {
    let service = service.unwrap_or_else(|| Arc::new(InMemorySessionService::new()));
    Executor::new(ExecutorConfig {
        output_mode: mode,
        runner_config: Some(runner_config(agent, service)),
        run_config: RunConfig {
            streaming_mode: StreamingMode::Sse,
            ..Default::default()
        },
        ..Default::default()
    })
}

/// Go `filterPartial`.
fn filter_partial(parts: &[a2a::Part]) -> Vec<a2a::Part> {
    let key = to_a2a_meta_key("partial");
    parts
        .iter()
        .filter(|p| p.metadata.as_ref().and_then(|m| m.get(&key)) != Some(&Value::Bool(true)))
        .cloned()
        .collect()
}

/// Go `findLongRunningCall`.
fn find_long_running_call(parts: &[Part]) -> (Option<FunctionCall>, Option<FunctionResponse>) {
    let calls: Vec<_> = parts
        .iter()
        .filter_map(|p| p.function_call.clone())
        .collect();
    let responses: Vec<_> = parts
        .iter()
        .filter_map(|p| p.function_response.clone())
        .collect();
    assert!(calls.len() <= 1, "got {} calls, want 1", calls.len());
    assert!(
        responses.len() <= 1,
        "got {} responses, want 1",
        responses.len()
    );
    (calls.into_iter().next(), responses.into_iter().next())
}

/// Go `createLongRunningToolApproval`.
fn create_long_running_tool_approval(pending: &FunctionResponse) -> Part {
    let pending_approval: Approval =
        serde_json::from_value(Value::Object(pending.response.clone())).unwrap();
    let mut response = Part::from_function_response(
        APPROVAL_TOOL_NAME,
        obj(serde_json::to_value(Approval {
            status: APPROVAL_STATUS_APPROVED.into(),
            ticket_id: pending_approval.ticket_id,
        })
        .unwrap()),
    );
    if let Some(fr) = response.function_response.as_mut() {
        fr.id = pending.id.clone();
    }
    response
}

/// Go `createToolConfirmationApproval`.
fn create_tool_confirmation_approval(tool_call: &FunctionCall) -> Part {
    let ticket_id = tool_call
        .args
        .get("toolConfirmation")
        .and_then(|tc| tc.get("payload"))
        .and_then(|p| p.get("ticket_id"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!("toolCall = {tool_call:?}, want toolConfirmation.payload.ticket_id")
        })
        .to_owned();
    Part {
        function_response: Some(FunctionResponse {
            id: tool_call.id.clone(),
            name: tool_call.name.clone(),
            response: obj(json!({"confirmed": true, "payload": {"ticket_id": ticket_id}})),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// How an approval is created from the pending call (Go test-case `createApproval`).
type CreateApproval = fn(&FunctionCall, Option<&FunctionResponse>) -> Part;

/// JSON for comparison with Go's options: message IDs ignored (`IgnoreFields(a2a.Message{},
/// "ID")`) and data-map entries whose key ends in `id` ignored (`IgnoreMapEntries`), empty and
/// absent treated alike (`EquateEmpty`).
fn comparable(v: &impl serde::Serialize) -> Value {
    fn walk(v: &mut Value, in_data: bool) {
        match v {
            Value::Object(m) => {
                m.remove("messageId");
                if in_data {
                    m.retain(|k, _| !k.ends_with("id"));
                }
                m.retain(|_, v| {
                    !matches!(v, Value::Null)
                        && !matches!(v, Value::Array(a) if a.is_empty())
                        && !matches!(v, Value::Object(o) if o.is_empty())
                });
                for (k, child) in m.iter_mut() {
                    walk(child, in_data || k == "data");
                }
            }
            Value::Array(a) => a.iter_mut().for_each(|c| walk(c, in_data)),
            _ => {}
        }
    }
    let mut v = serde_json::to_value(v).unwrap();
    walk(&mut v, false);
    v
}

// parity: adapted: ADK-Go keeps a status message in a task's history when the status changes; a2a-rs's task store keeps the user messages only, so the history comparison checks the user messages
#[tokio::test(flavor = "multi_thread")]
async fn test_a2_a_input_required() {
    struct Case {
        name: &'static str,
        tool: fn() -> Arc<dyn gadk_agent::Tool>,
        create_approval: CreateApproval,
        want_first_artifact_parts: Vec<a2a::Part>,
        want_second_artifact_parts: Vec<a2a::Part>,
    }
    let cases = [
        Case {
            name: "long-running",
            tool: new_long_running_tool,
            create_approval: |_call, pending| create_long_running_tool_approval(pending.unwrap()),
            want_first_artifact_parts: vec![
                text_part(MODEL_TEXT_REQUIRES_APPROVAL),
                text_part(MODEL_TEXT_WAITING_FOR_APPROVAL),
            ],
            want_second_artifact_parts: vec![text_part(MODEL_TEXT_TASK_COMPLETE)],
        },
        Case {
            name: "tool confirmation",
            tool: new_tool_confirmation,
            create_approval: |call, _pending| create_tool_confirmation_approval(call),
            want_first_artifact_parts: vec![
                text_part(MODEL_TEXT_REQUIRES_APPROVAL),
                data_part(
                    json!({"name": APPROVAL_TOOL_NAME}),
                    &[
                        (&to_a2a_meta_key("is_long_running"), json!(false)),
                        (&to_a2a_meta_key("type"), json!("function_call")),
                    ],
                ),
                data_part(
                    json!({"name": APPROVAL_TOOL_NAME, "response": {"status": APPROVAL_STATUS_PENDING}}),
                    &[(&to_a2a_meta_key("type"), json!("function_response"))],
                ),
            ],
            want_second_artifact_parts: vec![
                data_part(
                    json!({"name": APPROVAL_TOOL_NAME, "response": {"status": APPROVAL_STATUS_VERIFIED}}),
                    &[(&to_a2a_meta_key("type"), json!("function_response"))],
                ),
                text_part(MODEL_TEXT_TASK_COMPLETE),
            ],
        },
    ];

    for tc in cases {
        let name = tc.name;
        // Server
        let input_requesting_agent = new_input_requesting_agent("agent-b", (tc.tool)());
        let executor = new_agent_executor(input_requesting_agent, None, OutputMode::ArtifactPerRun);
        let server = start_a2a_server(executor).await;

        // Client
        let client = new_a2a_client(&server).await;

        // Initial message triggers input required
        let msg1 = new_message(a2a::Role::User, vec![text_part("Perform important task!")]);
        let task1 = must_send_message(&client, &msg1).await;
        assert_eq!(task1.status.state, a2a::TaskState::InputRequired, "{name}");
        assert_eq!(
            task1.artifacts.as_deref().unwrap_or_default().len(),
            1,
            "{name}"
        );

        // Incomplete followup keeps the task in input-required
        let msg2 = new_message_for_task(
            a2a::Role::User,
            &task1,
            vec![text_part("Is it really necessary?")],
        );
        let task2 = must_send_message(&client, &msg2).await;
        assert_eq!(task2.status.state, a2a::TaskState::InputRequired, "{name}");
        assert_eq!(
            task2.artifacts.as_deref().unwrap_or_default().len(),
            1,
            "{name}"
        );

        // Verify that the error message is present: the last part.
        let status2 = task2.status.message.clone().unwrap();
        assert!(status2.parts.len() >= 2, "{name}: {status2:?}");
        let text = part_text(status2.parts.last().unwrap());
        assert!(!text.is_empty(), "{name}: last part is not a text part");
        assert!(text.contains("no input provided"), "{name}: {text:?}");

        // Another incomplete followup does not accumulate error messages
        let msg2a =
            new_message_for_task(a2a::Role::User, &task1, vec![text_part("Still debating?")]);
        let task2a = must_send_message(&client, &msg2a).await;
        assert_eq!(task2a.status.state, a2a::TaskState::InputRequired, "{name}");
        let validation_errors = task2a
            .status
            .message
            .as_ref()
            .unwrap()
            .parts
            .iter()
            .filter(|p| part_text(p).contains("no input provided"))
            .count();
        assert_eq!(validation_errors, 1, "{name}");

        let (tool_call, pending_response) =
            find_long_running_call(&to_genai_parts(&status2.parts).unwrap());
        let tool_call = tool_call.unwrap();
        let approved_response = (tc.create_approval)(&tool_call, pending_response.as_ref());

        let msg3 = new_message_for_task(
            a2a::Role::User,
            &task2,
            vec![
                text_part("LGTM"),
                to_a2a_parts(&[approved_response], std::slice::from_ref(&tool_call.id))
                    .unwrap()
                    .remove(0),
            ],
        );
        let task3 = must_send_message(&client, &msg3).await;
        assert_eq!(task3.status.state, a2a::TaskState::Completed, "{name}");

        // Verify the final task state
        let artifacts = task3.artifacts.clone().unwrap_or_default();
        assert_eq!(artifacts.len(), 2, "{name}");

        let got_history: Vec<Value> = task3.history.iter().flatten().map(comparable).collect();
        let want_history: Vec<Value> = [&msg1, &msg2, &msg2a, &msg3]
            .into_iter()
            .map(comparable)
            .collect();
        assert_eq!(got_history, want_history, "{name}: history");

        let got_first = &without_partial_artifacts(artifacts.clone())[0].parts;
        assert_eq!(
            comparable(got_first),
            comparable(&tc.want_first_artifact_parts),
            "{name}: first artifact"
        );
        assert_eq!(
            comparable(&artifacts[1].parts),
            comparable(&tc.want_second_artifact_parts),
            "{name}: second artifact"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn test_a2_a_runner_resumes_mixed_function_responses() {
    for name in ["peer-first", "local-first"] {
        let peer = new_input_requesting_agent("peer", new_long_running_tool());
        let server =
            start_a2a_server(new_agent_executor(peer, None, OutputMode::ArtifactPerRun)).await;
        let client = new_a2a_client(&server).await;

        let remote = new_a2a_remote_agent("remote", &server.url);
        let store: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
        let created = store
            .create(gadk_session::CreateRequest {
                app_name: "local".into(),
                user_id: "user".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        let mut cfg = runner_config(remote, Arc::clone(&store));
        cfg.app_name = "local".into();
        let runner = gadk_engine::runner::Runner::new(cfg).unwrap();
        let config = RunConfig {
            streaming_mode: StreamingMode::None,
            ..Default::default()
        };

        let mut task_id = String::new();
        let mut pending: Option<FunctionResponse> = None;
        let mut stream = runner.run(
            "user",
            created.id(),
            Some(Content::from_text("perform task", gadk_genai::ROLE_USER)),
            config.clone(),
            gadk_engine::runner::RunOptions::default(),
        );
        while let Some(item) = stream.next().await {
            let event = item.unwrap_or_else(|err| panic!("{name}: initial Run() error = {err}"));
            assert!(
                event.response.error_message.is_empty(),
                "{name}: initial Run() reported error: {}",
                event.response.error_message
            );
            let (id, _) = get_a2a_task_info(&event);
            if !id.is_empty() {
                task_id = id;
            }
            for response in function_responses(event.response.content.as_ref()) {
                if response.name == APPROVAL_TOOL_NAME {
                    pending = Some(response.clone());
                }
            }
        }
        drop(stream);
        let Some(pending) = pending.filter(|_| !task_id.is_empty()) else {
            panic!("{name}: initial Run() did not expose a pending remote tool and task");
        };
        let get_task = |id: String| a2a::GetTaskRequest {
            id,
            history_length: None,
            tenant: None,
        };
        let before = client.get_task(&get_task(task_id.clone())).await;
        assert!(
            before
                .as_ref()
                .is_ok_and(|t| t.status.state == a2a::TaskState::InputRequired),
            "{name}: initial remote task is not input-required: {before:?}"
        );

        let mut local = Part::from_function_response("local_tool", obj(json!({"ok": true})));
        if let Some(fr) = local.function_response.as_mut() {
            fr.id = "fc-local".into();
        }
        let mut parts = vec![create_long_running_tool_approval(&pending), local];
        if name == "local-first" {
            parts.swap(0, 1);
        }
        let mut failures = Vec::new();
        let mut completed = false;
        let mut stream = runner.run(
            "user",
            created.id(),
            Some(Content::from_parts(parts, gadk_genai::ROLE_USER)),
            config,
            gadk_engine::runner::RunOptions::default(),
        );
        while let Some(item) = stream.next().await {
            let event = match item {
                Ok(event) => event,
                Err(err) => {
                    failures.push(format!("resume Run() error = {err}"));
                    break;
                }
            };
            if !event.response.error_message.is_empty() {
                failures.push(format!(
                    "resume Run() reported error: {}",
                    event.response.error_message
                ));
            }
            if let Some(content) = &event.response.content {
                completed = completed
                    || content
                        .parts
                        .iter()
                        .any(|p| p.text == MODEL_TEXT_TASK_COMPLETE);
            }
        }
        drop(stream);
        if !completed {
            failures.push("Runner did not receive the remote task's completion result".into());
        }
        let after = client.get_task(&get_task(task_id.clone())).await.unwrap();
        if after.status.state != a2a::TaskState::Completed {
            failures.push(format!(
                "original remote task state = {:?}, want Completed",
                after.status.state
            ));
        }
        // Go's deferred cleanup: cancel the remote task unless it finished.
        if !after.status.state.is_terminal() {
            let _ = client
                .cancel_task(&a2a::CancelTaskRequest {
                    id: task_id.clone(),
                    metadata: None,
                    tenant: None,
                })
                .await;
        }
        assert!(failures.is_empty(), "{name}: {failures:#?}");
    }
}

/// Go `assertADKExtensionMeta`.
fn assert_adk_extension_meta(
    subject: &str,
    meta: Option<&std::collections::HashMap<String, Value>>,
) {
    let got = meta.and_then(|m| m.get(ADK_EXTENSION_URI));
    assert_eq!(
        got,
        Some(&Value::Bool(true)),
        "{subject}: metadata[{ADK_EXTENSION_URI:?}] in {meta:?}"
    );
}

/// One streamed turn: the task (submitted or a snapshot) and the last status update.
async fn stream_turn(
    client: &Client,
    msg: a2a::Message,
) -> (Option<a2a::Task>, a2a::TaskStatusUpdateEvent) {
    let req = a2a::SendMessageRequest {
        message: msg,
        configuration: None,
        metadata: None,
        tenant: None,
    };
    let (mut task, mut last_status) = (None, None);
    let mut stream = client.send_streaming_message(&req).await.unwrap();
    while let Some(event) = stream.next().await {
        match event.unwrap() {
            a2a::StreamResponse::Task(t) => task = Some(t),
            a2a::StreamResponse::StatusUpdate(u) => last_status = Some(u),
            _ => {}
        }
    }
    (task, last_status.expect("a status update"))
}

// parity: adapted: a2a-rs's task store never merges status-update metadata into the stored task, so the SendMessage/GetTask task lacks the extension key; the port reads it where a2a-rs carries it, on each turn's streamed final status update
#[tokio::test(flavor = "multi_thread")]
async fn test_a2_a_task_carries_adk_extension_meta() {
    let input_requesting_agent = new_input_requesting_agent("agent-b", new_long_running_tool());
    let server = start_a2a_server(new_agent_executor(
        input_requesting_agent,
        None,
        OutputMode::ArtifactPerRun,
    ))
    .await;
    let client = new_a2a_client(&server).await;

    let msg1 = new_message(a2a::Role::User, vec![text_part("Perform important task!")]);
    let (task1, status1) = stream_turn(&client, msg1).await;
    let task1 = task1.expect("the submitted task");
    assert_eq!(status1.status.state, a2a::TaskState::InputRequired);
    // The regressing case: the long-running call lives in the status message, while the
    // artifacts hold the model's text.
    assert_adk_extension_meta("input-required task", status1.metadata.as_ref());

    let status = status1.status.message.as_ref().unwrap();
    let (tool_call, pending_response) =
        find_long_running_call(&to_genai_parts(&status.parts).unwrap());
    let approval = create_long_running_tool_approval(&pending_response.unwrap());
    let msg2 = new_message_for_task(
        a2a::Role::User,
        &task1,
        vec![
            text_part("LGTM"),
            to_a2a_parts(&[approval], &[tool_call.unwrap().id])
                .unwrap()
                .remove(0),
        ],
    );
    let (_, status2) = stream_turn(&client, msg2).await;
    assert_eq!(status2.status.state, a2a::TaskState::Completed);
    // The extension describes every response, not just input-required ones, and it has to
    // survive the follow-up turn that resolves the task.
    assert_adk_extension_meta("completed task", status2.metadata.as_ref());
}

#[tokio::test(flavor = "multi_thread")]
async fn test_a2_a_task_carries_adk_extension_meta_when_streaming() {
    let input_requesting_agent = new_input_requesting_agent("agent-b", new_long_running_tool());
    let server = start_a2a_server(new_agent_executor(
        input_requesting_agent,
        None,
        OutputMode::ArtifactPerRun,
    ))
    .await;
    let client = new_a2a_client(&server).await;

    let msg = new_message(a2a::Role::User, vec![text_part("Perform important task!")]);
    let req = a2a::SendMessageRequest {
        message: msg,
        configuration: None,
        metadata: None,
        tenant: None,
    };
    let mut status_updates = 0;
    let mut stream = client.send_streaming_message(&req).await.unwrap();
    while let Some(event) = stream.next().await {
        match event.unwrap() {
            a2a::StreamResponse::Task(task) => {
                assert_adk_extension_meta("submitted task", task.metadata.as_ref());
            }
            a2a::StreamResponse::StatusUpdate(update) => {
                status_updates += 1;
                assert_adk_extension_meta(
                    &format!("status update {status_updates} ({:?})", update.status.state),
                    update.metadata.as_ref(),
                );
            }
            _ => {}
        }
    }
    assert!(
        status_updates > 0,
        "no task status updates, want at least one"
    );
}

// parity: adapted: ADK-Go keeps a status message in a task's history when the status changes; a2a-rs's task store keeps the user messages only, so the history comparison checks the user messages
#[tokio::test(flavor = "multi_thread")]
#[ignore = "8a bug: transfer_to_agent's empty result is sent as `{}` instead of `{\"result\": null}` (Go 1ced38ba)"]
async fn test_a2_a_multi_hop_input_required() {
    const REMOTE_AGENT_NAME: &str = "remote-agent-B";
    struct Case {
        name: &'static str,
        tool: fn() -> Arc<dyn gadk_agent::Tool>,
        create_approval: CreateApproval,
        want_first_artifact_parts: Vec<a2a::Part>,
        want_second_artifact_parts: Vec<a2a::Part>,
    }
    let transfer_parts = || {
        vec![
            Part::from_text(MODEL_TEXT_ROOT_TRANSFER),
            Part::from_function_call(
                TRANSFER_TOOL_NAME,
                obj(json!({"agent_name": REMOTE_AGENT_NAME})),
            ),
            Part::from_function_response(TRANSFER_TOOL_NAME, obj(json!({"result": null}))),
            Part::from_text(MODEL_TEXT_REQUIRES_APPROVAL),
        ]
    };
    let cases = [
        Case {
            name: "long-running",
            tool: new_long_running_tool,
            create_approval: |_call, pending| create_long_running_tool_approval(pending.unwrap()),
            want_first_artifact_parts: to_a2a_parts(
                &[
                    transfer_parts(),
                    vec![Part::from_text(MODEL_TEXT_WAITING_FOR_APPROVAL)],
                ]
                .concat(),
                &[],
            )
            .unwrap(),
            want_second_artifact_parts: vec![text_part(MODEL_TEXT_TASK_COMPLETE)],
        },
        Case {
            name: "tool confirmation",
            tool: new_tool_confirmation,
            create_approval: |call, _pending| create_tool_confirmation_approval(call),
            want_first_artifact_parts: to_a2a_parts(
                &[
                    transfer_parts(),
                    vec![
                        Part::from_function_call(APPROVAL_TOOL_NAME, Map::new()),
                        Part::from_function_response(
                            APPROVAL_TOOL_NAME,
                            obj(json!({"status": APPROVAL_STATUS_PENDING})),
                        ),
                    ],
                ]
                .concat(),
                &[],
            )
            .unwrap(),
            want_second_artifact_parts: vec![
                data_part(
                    json!({"name": APPROVAL_TOOL_NAME, "response": {"status": APPROVAL_STATUS_VERIFIED}}),
                    &[(&to_a2a_meta_key("type"), json!("function_response"))],
                ),
                text_part(MODEL_TEXT_TASK_COMPLETE),
            ],
        },
    ];

    for tc in cases {
        let name = tc.name;
        // Server B
        let input_requesting_agent = new_input_requesting_agent("agent-b", (tc.tool)());
        let executor_b =
            new_agent_executor(input_requesting_agent, None, OutputMode::ArtifactPerEvent);
        let server_b = start_a2a_server(executor_b).await;

        // Server A
        let remote_agent = new_a2a_remote_agent(REMOTE_AGENT_NAME, &server_b.url);
        let root_agent = new_root_agent("root", remote_agent);
        let executor_a = new_agent_executor(root_agent, None, OutputMode::ArtifactPerRun);
        let server_a = start_a2a_server(executor_a).await;

        // Client for Server A
        let client = new_a2a_client(&server_a).await;

        // Initial message triggers input required
        let msg1 = new_message(
            a2a::Role::User,
            vec![text_part("Hello, perform important task!")],
        );
        let task1 = must_send_message(&client, &msg1).await;
        assert_eq!(task1.status.state, a2a::TaskState::InputRequired, "{name}");

        // Incomplete followup keeps the task in input-required
        let msg2 = new_message_for_task(
            a2a::Role::User,
            &task1,
            vec![text_part("Is it really necessary?")],
        );
        let task2 = must_send_message(&client, &msg2).await;
        assert_eq!(task2.status.state, a2a::TaskState::InputRequired, "{name}");

        // Required input gets delivered
        let status2 = task2.status.message.clone().unwrap();
        let (tool_call, pending_response) =
            find_long_running_call(&to_genai_parts(&filter_partial(&status2.parts)).unwrap());
        let tool_call = tool_call.unwrap();
        let approved_response = (tc.create_approval)(&tool_call, pending_response.as_ref());
        let msg3 = new_message_for_task(
            a2a::Role::User,
            &task2,
            vec![
                text_part("LGTM"),
                to_a2a_parts(&[approved_response], &[]).unwrap().remove(0),
            ],
        );
        let task3 = must_send_message(&client, &msg3).await;
        assert_eq!(task3.status.state, a2a::TaskState::Completed, "{name}");

        // Verify the task on server A
        let got_history: Vec<Value> = task3.history.iter().flatten().map(comparable).collect();
        let want_history: Vec<Value> = [&msg1, &msg2, &msg3].into_iter().map(comparable).collect();
        assert_eq!(got_history, want_history, "{name}: history");

        let artifacts = task3.artifacts.clone().unwrap_or_default();
        assert_eq!(
            comparable(&filter_partial(&artifacts[0].parts)),
            comparable(&tc.want_first_artifact_parts),
            "{name}: first artifact"
        );
        assert_eq!(
            comparable(&filter_partial(
                &without_partial_artifacts(artifacts)[1].parts
            )),
            comparable(&tc.want_second_artifact_parts),
            "{name}: second artifact"
        );
    }
}

/// Counts the drops of the streams it guards: a2a-rs has no executor cleanup hook (Go's
/// `a2asrv` cleanup interface), so the end of an execute or cancel stream stands for it.
struct DropCounter(Arc<AtomicUsize>);

impl Drop for DropCounter {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

/// `stream` that bumps `counter` when it ends or is dropped.
fn counted(stream: A2AStream, counter: Arc<AtomicUsize>) -> A2AStream {
    let guard = DropCounter(counter);
    stream
        .chain(futures::stream::poll_fn(move |_| {
            let _ = &guard;
            std::task::Poll::Ready(None)
        }))
        .boxed()
}

/// Waits until `cond` holds, for at most 10 s (Go `testutil.AwaitN`'s generous deadline).
async fn await_until(what: &str, cond: impl Fn() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !cond() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

// parity: adapted: a2a-rs has no executor cleanup hook, so server B's cleanup is the end of its execute and cancel streams (a drop guard); a2a-rs drops the execute stream at the next event after the cancel instead of cancelling its context, so server B streams one artifact and then waits for its cancel before yielding the next (a2a-rs fails a streaming subscription whose execution event races the cancel); executor A sets StreamingMode::Sse, Go's zero StreamingMode streams
#[tokio::test(flavor = "multi_thread")]
async fn test_a2_a_cleanup_propagation() {
    let remote_task_id: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let remote_cleanups = Arc::new(AtomicUsize::new(0));
    // Artifact text the mock subagent streams; the cancel step keys off it.
    const REMOTE_ARTIFACT_TEXT: &str = "remote-subagent-working";
    // Signalled by server B's cancel.
    let remote_canceled = Arc::new(tokio::sync::Notify::new());
    // The remote A2A server publishes a submitted task and generates artifact updates until its
    // stream is dropped. After the first one it waits for its task's cancel: a2a-rs fails the
    // streaming subscription with "task … is in terminal state Canceled and cannot transition to
    // Working" when an execution event is in flight as the cancel lands (Go serializes the cancel
    // through the execution's event queue), and executor A forwards every artifact.
    let server_b = start_a2a_server(MockExecutor {
        cancel: Some({
            let remote_cleanups = Arc::clone(&remote_cleanups);
            let remote_canceled = Arc::clone(&remote_canceled);
            Arc::new(move |ctx: a2a_server::ExecutorContext| {
                remote_canceled.notify_one();
                let update = status_update(&ctx.task_id, &ctx.context_id, a2a::TaskState::Canceled);
                counted(
                    futures::stream::iter([Ok(a2a::StreamResponse::StatusUpdate(update))]).boxed(),
                    Arc::clone(&remote_cleanups),
                )
            })
        }),
        execute: Some({
            let remote_cleanups = Arc::clone(&remote_cleanups);
            let remote_task_id = Arc::clone(&remote_task_id);
            let remote_canceled = Arc::clone(&remote_canceled);
            Arc::new(move |ctx: a2a_server::ExecutorContext| {
                *remote_task_id.lock() = Some(ctx.task_id.clone());
                let submitted = submitted_task(&ctx);
                let (task_id, context_id) = (ctx.task_id.clone(), ctx.context_id.clone());
                let remote_canceled = Arc::clone(&remote_canceled);
                let stream = async_stream::stream! {
                    yield Ok(a2a::StreamResponse::Task(submitted));
                    let mut first = true;
                    loop {
                        if !first {
                            remote_canceled.notified().await;
                        }
                        first = false;
                        let update = a2a::TaskArtifactUpdateEvent {
                            task_id: task_id.clone(),
                            context_id: context_id.clone(),
                            artifact: a2a::Artifact {
                                artifact_id: a2a::new_artifact_id(),
                                name: None,
                                description: None,
                                parts: vec![a2a::Part::text(REMOTE_ARTIFACT_TEXT)],
                                metadata: None,
                                extensions: None,
                            },
                            append: None,
                            last_chunk: None,
                            metadata: None,
                        };
                        yield Ok(a2a::StreamResponse::ArtifactUpdate(update));
                        tokio::time::sleep(Duration::from_millis(1)).await;
                    }
                };
                counted(stream.boxed(), Arc::clone(&remote_cleanups))
            })
        }),
    })
    .await;

    // The root server connects to server B through a remote subagent.
    let remote_agent_b = new_a2a_remote_agent("remote-agent-b", &server_b.url);
    let root_a = new_root_agent("agent-b", remote_agent_b);
    let executor_cleanups = Arc::new(AtomicUsize::new(0));
    let executor_a = Executor::new(ExecutorConfig {
        output_mode: OutputMode::ArtifactPerRun,
        runner_config: Some(runner_config(
            Arc::clone(&root_a),
            Arc::new(InMemorySessionService::new()),
        )),
        // Go's zero `RunConfig` has StreamingMode "" (not "none"), so its remote agent streams;
        // gadk's default is `StreamingMode::None` (a non-streaming send), so SSE is explicit.
        run_config: RunConfig {
            streaming_mode: StreamingMode::Sse,
            ..Default::default()
        },
        a2a_execution_cleanup_callback: Some({
            let executor_cleanups = Arc::clone(&executor_cleanups);
            Arc::new(move |_ctx, _cards, _result, _cause| {
                executor_cleanups.fetch_add(1, Ordering::SeqCst);
                Box::pin(async {})
            })
        }),
        ..Default::default()
    });
    let server_a = start_a2a_server(executor_a).await;
    let client = Arc::new(new_a2a_client(&server_a).await);

    // The streaming request: status updates go to the channel; `remote_streaming` flips when
    // the subagent's own output first reaches the client.
    let (status_tx, mut status_rx) = tokio::sync::mpsc::channel::<a2a::StreamResponse>(16);
    let remote_streaming = Arc::new(tokio::sync::Notify::new());
    let streaming = tokio::spawn({
        let client = Arc::clone(&client);
        let remote_streaming = Arc::clone(&remote_streaming);
        async move {
            let req = a2a::SendMessageRequest {
                message: new_message(a2a::Role::User, vec![text_part("work")]),
                configuration: None,
                metadata: None,
                tenant: None,
            };
            let mut seen_remote = false;
            let mut stream = client.send_streaming_message(&req).await.unwrap();
            while let Some(event) = stream.next().await {
                let event = event.unwrap();
                if let a2a::StreamResponse::ArtifactUpdate(update) = &event {
                    if !seen_remote
                        && update
                            .artifact
                            .parts
                            .iter()
                            .any(|p| part_text(p).contains(REMOTE_ARTIFACT_TEXT))
                    {
                        seen_remote = true;
                        remote_streaming.notify_one();
                    }
                    continue;
                }
                if status_tx.send(event).await.is_err() {
                    return;
                }
            }
        }
    });

    // Cancel only after the subagent's output reaches the client: before that the parent
    // doesn't know the subagent task ID, so cancellation can't propagate.
    let first_update = tokio::time::timeout(Duration::from_secs(10), status_rx.recv())
        .await
        .expect("first status update")
        .unwrap();
    let task_id = match &first_update {
        a2a::StreamResponse::Task(t) => t.id.clone(),
        a2a::StreamResponse::StatusUpdate(u) => u.task_id.clone(),
        other => panic!("first update = {other:?}"),
    };
    tokio::time::timeout(Duration::from_secs(10), remote_streaming.notified())
        .await
        .expect("remote subagent streaming");
    let cancel = tokio::spawn({
        let client = Arc::clone(&client);
        async move {
            client
                .cancel_task(&a2a::CancelTaskRequest {
                    id: task_id,
                    metadata: None,
                    tenant: None,
                })
                .await
                .unwrap()
        }
    });

    // The streaming sender gets a canceled status as its last update.
    let mut last = first_update;
    while let Ok(Some(event)) =
        tokio::time::timeout(Duration::from_secs(10), status_rx.recv()).await
    {
        last = event;
    }
    match &last {
        a2a::StreamResponse::StatusUpdate(u) => {
            assert_eq!(
                u.status.state,
                a2a::TaskState::Canceled,
                "last streaming update"
            )
        }
        other => panic!("type(lastStreamingUpdate) = {other:?}, want a status update"),
    }

    // The subagent's cleanup fires twice: once for the cancellation, once for the execution.
    await_until("remote cleanup", || {
        remote_cleanups.load(Ordering::SeqCst) >= 2
    })
    .await;
    let remote_task_id = remote_task_id
        .lock()
        .clone()
        .expect("server B remote task ID");
    await_until("executor cleanup", || {
        executor_cleanups.load(Ordering::SeqCst) >= 2
    })
    .await;

    let remote_client = new_a2a_client(&server_b).await;
    let remote_task = remote_client
        .get_task(&a2a::GetTaskRequest {
            id: remote_task_id,
            history_length: None,
            tenant: None,
        })
        .await
        .unwrap();
    assert_eq!(remote_task.status.state, a2a::TaskState::Canceled);

    // Join the cancel RPC and the streaming request.
    tokio::time::timeout(Duration::from_secs(10), cancel)
        .await
        .expect("cancel task")
        .unwrap();
    streaming.await.unwrap();
}

/// Go `newADKEventReplay`: an agent yielding `events` stamped with the invocation.
fn adk_event_replay(name: &str, events: Vec<Event>) -> Arc<dyn Agent> {
    let author = name.to_owned();
    let events = Arc::new(events);
    new_agent(AgentConfig {
        name: name.to_owned(),
        run: Some(Arc::new(move |ic| {
            let mut out = Vec::new();
            for ev in events.iter() {
                let mut ev = ev.clone();
                ev.invocation_id = ic.invocation_id().to_owned();
                ev.branch = ic.branch();
                ev.author = author.clone();
                out.push(Ok::<_, AgentError>(ev));
            }
            futures::stream::iter(out).boxed()
        })),
        ..Default::default()
    })
    .unwrap()
}

fn text_event(text: &str, partial: bool, turn_complete: bool) -> Event {
    Event {
        response: LlmResponse {
            content: Some(Content::from_text(text, ROLE_MODEL)),
            partial,
            turn_complete,
            ..Default::default()
        },
        ..Default::default()
    }
}

/// The httprr recording ADK-Go's test `name` replays (Go `newGeminiModel`: `testdata/<t.Name()
/// with / → _>.httprr`), as copied under `tests/testdata`.
fn recording(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/testdata/agent/remoteagent/v2/testdata")
        .join(format!("{}.httprr", name.replace('/', "_")))
}

/// Go's `AfterModelCallbacks` of the mid-response cases: the first `pass` responses pass
/// through, every later one is replaced by `replace`.
fn after_model_failing_after(
    pass: usize,
    replace: Result<LlmResponse, &'static str>,
) -> AfterModelCallback {
    let seen = Arc::new(AtomicUsize::new(0));
    Arc::new(move |_ctx, resp, _err| {
        let n = seen.fetch_add(1, Ordering::SeqCst);
        let out = if n < pass {
            Ok(None)
        } else {
            match &replace {
                Ok(r) => Ok(Some(r.clone())),
                Err(text) => Err(AgentError::Other((*text).into())),
            }
        };
        Box::pin(async move { (resp, out) })
    })
}

/// Go `TestA2ASingleHopFinalResponse`'s gemini agent.
async fn gemini_agent(test_name: &str, after: AfterModelCallback) -> Arc<dyn Agent> {
    let model =
        gadk_engine::testutil::gemini_test_model("gemini-2.5-flash", recording(test_name)).await;
    gadk_engine::llm_agent::new(LlmAgentConfig {
        name: "model-agent".into(),
        model: Some(model),
        after_model_callbacks: vec![after],
        instruction: "You are a helpful assistant.".into(),
        ..Default::default()
    })
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn test_a2_a_single_hop_final_response() {
    struct Case {
        name: &'static str,
        agent: Arc<dyn Agent>,
        session_service: Option<Arc<dyn SessionService>>,
        want_artifact_parts: Vec<a2a::Part>,
        want_state: a2a::TaskState,
        want_status_contain: &'static str,
        want_partial: bool,
    }
    let seq = |beep: Vec<Event>, boop: Vec<Event>| {
        gadk_engine::sequential_agent::new(gadk_engine::sequential_agent::SequentialAgentConfig {
            agent: AgentConfig {
                name: "root".into(),
                sub_agents: vec![
                    adk_event_replay("beep", beep),
                    adk_event_replay("boop", boop),
                ],
                ..Default::default()
            },
        })
        .unwrap()
    };
    let cases = vec![
        Case {
            name: "streaming",
            agent: seq(
                vec![
                    text_event("Hello,", true, false),
                    text_event(" I am beep!", true, true),
                    text_event("Hello, I am beep!", false, false),
                ],
                vec![
                    text_event("I am boop", true, false),
                    text_event(". We are", true, false),
                    text_event("here to help!", true, true),
                    text_event("I am boop. We are here to help!", false, false),
                ],
            ),
            session_service: None,
            want_state: a2a::TaskState::Completed,
            want_artifact_parts: vec![
                text_part("Hello, I am beep!"),
                text_part("I am boop. We are here to help!"),
            ],
            want_status_contain: "",
            want_partial: true,
        },
        Case {
            name: "non-streaming",
            agent: seq(
                vec![text_event("Hello, I am beep!", false, false)],
                vec![text_event("I am boop. We are here to help!", false, false)],
            ),
            session_service: None,
            want_state: a2a::TaskState::Completed,
            want_artifact_parts: vec![
                text_part("Hello, I am beep!"),
                text_part("I am boop. We are here to help!"),
            ],
            want_status_contain: "",
            want_partial: false,
        },
        Case {
            name: "internal error",
            agent: new_agent(AgentConfig {
                name: "internal_error".into(),
                run: Some(Arc::new(|_ic| futures::stream::empty().boxed())),
                ..Default::default()
            })
            .unwrap(),
            session_service: Some(session_service(true)),
            want_status_contain: "failed to create a session: session creation failed",
            want_state: a2a::TaskState::Failed,
            want_artifact_parts: vec![],
            want_partial: false,
        },
        Case {
            name: "llm mid-response error response",
            agent: gemini_agent(
                "TestA2ASingleHopFinalResponse/llm_mid-response_error_response",
                after_model_failing_after(
                    2,
                    Ok(LlmResponse {
                        error_code: "500".into(),
                        error_message: "Model Failed!".into(),
                        ..Default::default()
                    }),
                ),
            )
            .await,
            session_service: None,
            want_status_contain: "Model Failed!",
            want_state: a2a::TaskState::Failed,
            want_artifact_parts: vec![],
            want_partial: true,
        },
        Case {
            name: "llm mid-response error",
            agent: gemini_agent(
                "TestA2ASingleHopFinalResponse/llm_mid-response_error",
                after_model_failing_after(2, Err("connection error!")),
            )
            .await,
            session_service: None,
            want_status_contain: "connection error!",
            want_state: a2a::TaskState::Failed,
            want_artifact_parts: vec![],
            want_partial: true,
        },
    ];

    for tc in cases {
        let name = tc.name;
        let executor = new_agent_executor(tc.agent, tc.session_service, OutputMode::ArtifactPerRun);
        let server = start_a2a_server(executor).await;
        let client = new_a2a_client(&server).await;
        let msg = new_message(
            a2a::Role::User,
            vec![text_part("Tell me about the current weather")],
        );
        let task = must_send_message(&client, &msg).await;
        assert_eq!(task.status.state, tc.want_state, "{name}: {task:?}");

        let artifacts = task.artifacts.clone().unwrap_or_default();
        let non_partial = without_partial_artifacts(artifacts.clone());
        let want_response = !tc.want_artifact_parts.is_empty();
        if want_response {
            assert_eq!(non_partial.len(), 1, "{name}");
            assert_eq!(
                comparable(&non_partial[0].parts),
                comparable(&tc.want_artifact_parts),
                "{name}: artifact parts"
            );
        }

        if !tc.want_status_contain.is_empty() {
            let msg = task.status.message.as_ref().expect("a status message");
            assert_eq!(msg.parts.len(), 1, "{name}: {msg:?}");
            assert!(
                part_text(&msg.parts[0]).contains(tc.want_status_contain),
                "{name}: status message {:?}, want text containing {:?}",
                msg.parts[0],
                tc.want_status_contain
            );
        }

        if !tc.want_partial {
            if want_response {
                assert_eq!(artifacts.len(), 1, "{name}");
            }
            continue;
        }

        let want_artifact_count = if want_response { 2 } else { 1 };
        if want_response {
            assert_eq!(artifacts.len(), want_artifact_count, "{name}");
        }
        let partial_artifact = if is_partial(artifacts[0].metadata.as_ref()) {
            &artifacts[0]
        } else {
            &artifacts[1]
        };
        let want_partial_parts = vec![data_part(
            json!({}),
            &[(&to_a2a_meta_key("partial"), json!(true))],
        )];
        assert_eq!(
            serde_json::to_value(&partial_artifact.parts).unwrap(),
            serde_json::to_value(&want_partial_parts).unwrap(),
            "{name}: partial artifact parts"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn test_a2_a_remote_agent_streaming_gemini_success() {
    // Server B with a replayable LLM agent
    let model = gadk_engine::testutil::gemini_test_model(
        "gemini-2.5-flash",
        recording("TestA2ARemoteAgentStreamingGeminiSuccess"),
    )
    .await;
    let model_agent = gadk_engine::llm_agent::new(LlmAgentConfig {
        name: "model-agent".into(),
        model: Some(model),
        instruction: "You are a helpful assistant.".into(),
        ..Default::default()
    })
    .unwrap();
    let executor_b = new_agent_executor(model_agent, None, OutputMode::ArtifactPerEvent);
    let server_b = start_a2a_server(executor_b).await;

    // Server A with the remote agent
    let remote_agent = new_a2a_remote_agent("remote-agent", &server_b.url);
    let service_a: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    let executor_a = new_agent_executor(
        Arc::clone(&remote_agent),
        Some(Arc::clone(&service_a)),
        OutputMode::ArtifactPerRun,
    );
    let server_a = start_a2a_server(executor_a).await;

    let client = new_a2a_client(&server_a).await;
    let mut msg = new_message(
        a2a::Role::User,
        vec![text_part("tell me about the capital of Poland")],
    );
    let context_id = a2a::new_context_id();
    msg.context_id = Some(context_id.clone());

    // Make a streaming request and aggregate the results
    let req = a2a::SendMessageRequest {
        message: msg,
        configuration: None,
        metadata: None,
        tenant: None,
    };
    let mut task_id = String::new();
    let (mut partial_text, mut final_text) = (String::new(), String::new());
    let mut stream = client.send_streaming_message(&req).await.unwrap();
    while let Some(event) = stream.next().await {
        let event = event.unwrap();
        if let a2a::StreamResponse::ArtifactUpdate(update) = &event {
            if is_partial(update.metadata.as_ref()) && update.last_chunk == Some(true) {
                assert_eq!(update.artifact.parts.len(), 1, "final partial update parts");
                let empty = matches!(
                    &update.artifact.parts[0].content,
                    a2a::PartContent::Data(Value::Object(m)) if m.is_empty()
                );
                assert!(
                    empty,
                    "final partial update part {:?}",
                    update.artifact.parts[0]
                );
                continue;
            }
            if is_partial(update.metadata.as_ref()) {
                for p in &update.artifact.parts {
                    partial_text.push_str(part_text(p));
                }
                continue;
            }
            assert!(
                final_text.is_empty(),
                "got multiple non-partial updates, want 1"
            );
            final_text = part_text(&update.artifact.parts[0]).to_owned();
        }
        task_id = match &event {
            a2a::StreamResponse::Task(t) => t.id.clone(),
            a2a::StreamResponse::StatusUpdate(u) => u.task_id.clone(),
            a2a::StreamResponse::ArtifactUpdate(u) => u.task_id.clone(),
            a2a::StreamResponse::Message(m) => m.task_id.clone().unwrap_or_default(),
        };
    }

    // Check the streaming contents
    assert!(!final_text.is_empty(), "got empty final text");
    assert_eq!(partial_text, final_text, "final event text vs streaming");

    // Check the A2A task state
    let task = client
        .get_task(&a2a::GetTaskRequest {
            id: task_id,
            history_length: None,
            tenant: None,
        })
        .await
        .unwrap();
    assert_eq!(task.status.state, a2a::TaskState::Completed);

    // Check the session store state
    let session = service_a
        .get(GetRequest {
            app_name: remote_agent.name().to_owned(),
            user_id: format!("A2A_USER_{context_id}"),
            session_id: context_id.clone(),
            ..Default::default()
        })
        .await
        .unwrap();
    let events = session.events();
    assert_eq!(
        events.len(),
        3,
        "want [user-msg, response, turn-complete]: {events:?}"
    );
    assert_eq!(events[0].author, "user");
    assert!(
        events[2].response.turn_complete && events[2].response.content.is_none(),
        "last event {:?}",
        events[2]
    );
    let content = events[1].response.content.as_ref().unwrap();
    assert_eq!(content.parts.len(), 1);
    assert_eq!(content.parts[0].text, final_text);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_a2_a_remote_agent_streaming_gemini_error() {
    // Server B with a replayable LLM agent which fails after emitting some events
    const ERROR_MESSAGE: &str = "connection error!";
    let model = gadk_engine::testutil::gemini_test_model(
        "gemini-2.5-flash",
        recording("TestA2ARemoteAgentStreamingGeminiError"),
    )
    .await;
    let model_agent = gadk_engine::llm_agent::new(LlmAgentConfig {
        name: "model-agent".into(),
        model: Some(model),
        instruction: "You are a helpful assistant.".into(),
        after_model_callbacks: vec![after_model_failing_after(3, Err(ERROR_MESSAGE))],
        ..Default::default()
    })
    .unwrap();
    let executor_b = new_agent_executor(model_agent, None, OutputMode::ArtifactPerRun);
    let server_b = start_a2a_server(executor_b).await;

    // Server A with the remote agent
    let remote_agent = new_a2a_remote_agent("remote-agent", &server_b.url);
    let service_a: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    let executor_a = new_agent_executor(
        Arc::clone(&remote_agent),
        Some(Arc::clone(&service_a)),
        OutputMode::ArtifactPerRun,
    );
    let server_a = start_a2a_server(executor_a).await;

    let client = new_a2a_client(&server_a).await;
    let mut msg = new_message(
        a2a::Role::User,
        vec![text_part("tell me about the capital of Poland")],
    );
    let context_id = a2a::new_context_id();
    msg.context_id = Some(context_id.clone());

    let req = a2a::SendMessageRequest {
        message: msg,
        configuration: None,
        metadata: None,
        tenant: None,
    };
    let mut task_id = String::new();
    let mut stream = client.send_streaming_message(&req).await.unwrap();
    while let Some(event) = stream.next().await {
        task_id = match event.unwrap() {
            a2a::StreamResponse::Task(t) => t.id,
            a2a::StreamResponse::StatusUpdate(u) => u.task_id,
            a2a::StreamResponse::ArtifactUpdate(u) => u.task_id,
            a2a::StreamResponse::Message(m) => m.task_id.unwrap_or_default(),
        };
    }

    // Check the A2A task state
    let task = client
        .get_task(&a2a::GetTaskRequest {
            id: task_id,
            history_length: None,
            tenant: None,
        })
        .await
        .unwrap();
    assert_eq!(task.status.state, a2a::TaskState::Failed);
    let status = task.status.message.as_ref().expect("a status message");
    assert_eq!(status.parts.len(), 1, "{status:?}");
    assert!(
        part_text(&status.parts[0]).contains(ERROR_MESSAGE),
        "{status:?}"
    );
    let artifacts = task.artifacts.clone().unwrap_or_default();
    assert!(
        artifacts.len() == 1 && without_partial_artifacts(artifacts.clone()).is_empty(),
        "task artifacts = {artifacts:?}, want a single partial artifact"
    );
    assert!(
        matches!(artifacts[0].parts[0].content, a2a::PartContent::Data(_)),
        "task artifact = {:?}, want the reset partial artifact",
        artifacts[0]
    );

    // Check the session store state
    let session = service_a
        .get(GetRequest {
            app_name: remote_agent.name().to_owned(),
            user_id: format!("A2A_USER_{context_id}"),
            session_id: context_id.clone(),
            ..Default::default()
        })
        .await
        .unwrap();
    let events = session.events();
    assert_eq!(events.len(), 2, "{events:?}");
    assert!(
        events[1].response.error_message.contains(ERROR_MESSAGE),
        "event error message {:?}",
        events[1].response.error_message
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn test_a2_a_multi_hop_input_required_cancellation() {
    const REMOTE_AGENT_NAME: &str = "remote-agent-B";
    let remote_task_id: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let server_b = start_a2a_server(MockExecutor {
        execute: Some({
            let remote_task_id = Arc::clone(&remote_task_id);
            Arc::new(move |ctx: a2a_server::ExecutorContext| {
                *remote_task_id.lock() = Some(ctx.task_id.clone());
                let mut call = a2a::Part::data(json!({"id": "call-1", "name": "foo"}));
                call.metadata = Some(
                    [
                        ("adk_is_long_running".to_owned(), json!(true)),
                        ("adk_type".to_owned(), json!("function_call")),
                    ]
                    .into(),
                );
                let mut update =
                    status_update(&ctx.task_id, &ctx.context_id, a2a::TaskState::InputRequired);
                update.status.message = Some(a2a::Message::new(a2a::Role::Agent, vec![call]));
                futures::stream::iter([
                    Ok(a2a::StreamResponse::Task(submitted_task(&ctx))),
                    Ok(a2a::StreamResponse::StatusUpdate(update)),
                ])
                .boxed()
            })
        }),
        cancel: None,
    })
    .await;

    // Server A
    let remote_agent = new_a2a_remote_agent(REMOTE_AGENT_NAME, &server_b.url);
    let root_agent = new_root_agent("root", remote_agent);
    let executor_a = new_agent_executor(root_agent, None, OutputMode::ArtifactPerRun);
    let server_a = start_a2a_server(executor_a).await;

    // Send a message
    let client_a = new_a2a_client(&server_a).await;
    let msg1 = new_message(a2a::Role::User, vec![text_part("Hello")]);
    let task1 = must_send_message(&client_a, &msg1).await;
    assert_eq!(task1.status.state, a2a::TaskState::InputRequired);

    // Cancel the task on server A
    client_a
        .cancel_task(&a2a::CancelTaskRequest {
            id: task1.id.clone(),
            metadata: None,
            tenant: None,
        })
        .await
        .unwrap();

    // Server B's task gets canceled (by the executor's cleanup, after the cancel response).
    let remote_task_id = remote_task_id.lock().clone().expect("server B task ID");
    let client_b = new_a2a_client(&server_b).await;
    let get = || async {
        client_b
            .get_task(&a2a::GetTaskRequest {
                id: remote_task_id.clone(),
                history_length: None,
                tenant: None,
            })
            .await
            .unwrap()
    };
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let mut remote_task = get().await;
    while remote_task.status.state != a2a::TaskState::Canceled
        && tokio::time::Instant::now() < deadline
    {
        tokio::time::sleep(Duration::from_millis(10)).await;
        remote_task = get().await;
    }
    assert_eq!(remote_task.status.state, a2a::TaskState::Canceled);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_a2_a_multi_hop_structured_error_propagation() {
    // Server B with structured error serialization logic
    let structured_err = a2a::Part::data(json!({"error_type": "auth_required"}));
    let broken = new_agent(AgentConfig {
        name: "broken".into(),
        run: Some(Arc::new(|_ic| {
            futures::stream::iter([Err::<Event, _>(AgentError::Other(Box::new(Unauthorized)))])
                .boxed()
        })),
        ..Default::default()
    })
    .unwrap();
    let server_b = start_a2a_server(Executor::new(ExecutorConfig {
        runner_config: Some(runner_config(
            broken,
            Arc::new(InMemorySessionService::new()),
        )),
        after_execute_callback: Some({
            let structured_err = structured_err.clone();
            Arc::new(move |_ctx, final_event, err| {
                let unauthorized = err.is_some_and(is_unauthorized);
                if unauthorized && let Some(msg) = final_event.status.message.as_mut() {
                    msg.parts.push(structured_err.clone());
                }
                Box::pin(async { Ok(()) })
            })
        }),
        ..Default::default()
    }))
    .await;

    // Server A, default configuration
    let remote_agent = new_a2a_remote_agent("broken-remote", &server_b.url);
    let root_agent = new_root_agent("root", remote_agent);
    let executor_a = new_agent_executor(root_agent, None, OutputMode::ArtifactPerRun);
    let server_a = start_a2a_server(executor_a).await;

    // Send a message
    let client_a = new_a2a_client(&server_a).await;
    let msg1 = new_message(a2a::Role::User, vec![text_part("Hello")]);
    let task1 = must_send_message(&client_a, &msg1).await;
    assert_eq!(task1.status.state, a2a::TaskState::Failed, "{task1:?}");
    let parts = &task1.status.message.as_ref().unwrap().parts;
    assert_eq!(parts.len(), 2, "want [error text, extra parts]: {parts:?}");
    assert!(
        part_text(&parts[0]).contains("unauthorized"),
        "status.Message.Parts[0] = {:?}",
        parts[0]
    );
    assert_eq!(parts[1].content, structured_err.content);
}

/// Go `a2a.ErrUnauthorized` (a2a-rs has no such error value; its text is Go's).
#[derive(Debug)]
struct Unauthorized;

impl std::fmt::Display for Unauthorized {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("unauthorized")
    }
}

impl std::error::Error for Unauthorized {}

/// Go `errors.Is(err, a2a.ErrUnauthorized)`: the run error is the agent's [`Unauthorized`].
fn is_unauthorized(err: &crate::Error) -> bool {
    matches!(err, crate::Error::Agent(AgentError::Other(e)) if e.downcast_ref::<Unauthorized>().is_some())
}
