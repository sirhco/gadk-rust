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

//! Ports of ADK-Go `server/adka2a/v2/executor_test.go`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use a2a_server::AgentExecutor as _;
use futures::StreamExt as _;
use gadk_agent::{Agent, AgentConfig, AgentError, InvocationContext};
use gadk_engine::runner::{PluginConfig, RunnerConfig as EngineRunnerConfig};
use gadk_genai::{Content, Part};
use gadk_model::LlmResponse;
use gadk_platform::Platform;
use gadk_session::{
    CreateRequest, DeleteRequest, Event, GetRequest, InMemorySessionService, ListRequest, Session,
    SessionError, SessionService,
};
use parking_lot::Mutex;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

use super::server_adka2a_v2_processor::{
    model_partial_response_from_parts, model_response_from_parts, new_final_status_update,
};
use crate::Error;
use crate::convert::{
    ADK_EXTENSION_URI, DATA_PART_META_TYPE_KEY, DATA_PART_TYPE_FUNCTION_CALL, StreamResponseRef,
    to_a2a_meta_key,
};
use crate::remoteagent::{ClientContext, new_a2a_client_provider};
use crate::server::processor::{
    TaskInfo, error_from_response, new_artifact_event, new_artifact_update_event,
    new_message_for_task, new_status_update, to_task_failed_update_event,
};
use crate::server::{
    AfterEventCallback, AfterExecuteCallback, BeforeExecuteCallback, Executor, ExecutorConfig,
    OutputMode, RunnerConfig,
};
use crate::testutil::{ScriptedRunner, spawn_a2a_server};

/// Go `testSessionService`: an in-memory service whose `Create` can be made to fail.
struct TestSessionService {
    inner: InMemorySessionService,
    create_err: bool,
}

#[async_trait::async_trait]
impl SessionService for TestSessionService {
    async fn create(&self, req: CreateRequest) -> Result<Arc<dyn Session>, SessionError> {
        if self.create_err {
            return Err(SessionError::Backend("session creation failed".into()));
        }
        self.inner.create(req).await
    }
    async fn get(&self, req: GetRequest) -> Result<Arc<dyn Session>, SessionError> {
        self.inner.get(req).await
    }
    async fn list(&self, req: ListRequest) -> Result<Vec<Arc<dyn Session>>, SessionError> {
        self.inner.list(req).await
    }
    async fn delete(&self, req: DeleteRequest) -> Result<(), SessionError> {
        self.inner.delete(req).await
    }
    async fn append_event(
        &self,
        session: &dyn Session,
        event: &mut Event,
    ) -> Result<(), SessionError> {
        self.inner.append_event(session, event).await
    }
}

fn session_service(create_err: bool) -> Arc<dyn SessionService> {
    Arc::new(TestSessionService {
        inner: InMemorySessionService::new(),
        create_err,
    })
}

/// Go `agent.New(agent.Config{Name: "test", Run: run})`.
fn custom_agent(
    run: impl Fn(InvocationContext) -> gadk_agent::EventStream + Send + Sync + 'static,
) -> Arc<dyn Agent> {
    gadk_agent::new_agent(AgentConfig {
        name: "test".into(),
        run: Some(Arc::new(run)),
        ..Default::default()
    })
    .unwrap()
}

/// Go `newEventReplayAgent`.
fn new_event_replay_agent(events: Vec<Event>, fail_with: Option<&'static str>) -> Arc<dyn Agent> {
    custom_agent(move |_ctx| {
        let mut items: Vec<Result<Event, AgentError>> = events.iter().cloned().map(Ok).collect();
        if let Some(msg) = fail_with {
            items.push(Err(AgentError::other(msg)));
        }
        futures::stream::iter(items).boxed()
    })
}

/// Go `runner.Config{AppName: agent.Name(), Agent: agent, SessionService: service}`.
fn runner_config(
    agent: Arc<dyn Agent>,
    session_service: Arc<dyn SessionService>,
) -> EngineRunnerConfig {
    EngineRunnerConfig {
        app_name: agent.name().to_owned(),
        agent,
        session_service,
        artifact_service: None,
        memory_service: None,
        plugin_config: PluginConfig::default(),
        auto_create_session: false,
        platform: Platform::default(),
        compaction: None,
    }
}

fn response_event(response: LlmResponse) -> Event {
    Event {
        response,
        ..Default::default()
    }
}

fn authored(author: &str, response: LlmResponse) -> Event {
    Event {
        author: author.into(),
        ..response_event(response)
    }
}

fn task_info() -> TaskInfo {
    TaskInfo {
        task_id: a2a::new_task_id(),
        context_id: a2a::new_context_id(),
    }
}

fn stored_task(task: &TaskInfo) -> a2a::Task {
    a2a::Task {
        id: task.task_id.clone(),
        context_id: task.context_id.clone(),
        status: a2a::TaskStatus {
            state: a2a::TaskState::Unspecified,
            message: None,
            timestamp: None,
        },
        artifacts: None,
        history: None,
        metadata: None,
    }
}

/// Go `&a2asrv.ExecutorContext{TaskID, ContextID, Message, StoredTask}`.
fn request(
    task: &TaskInfo,
    message: Option<a2a::Message>,
    stored: bool,
) -> a2a_server::ExecutorContext {
    a2a_server::ExecutorContext {
        message,
        task_id: task.task_id.clone(),
        stored_task: stored.then(|| stored_task(task)),
        context_id: task.context_id.clone(),
        metadata: None,
        user: None,
        service_params: HashMap::new(),
        tenant: None,
    }
}

fn hi_msg() -> a2a::Message {
    a2a::Message::new(a2a::Role::User, vec![a2a::Part::text("hi")])
}

fn hi_msg_for_task(task: &TaskInfo) -> a2a::Message {
    new_message_for_task(a2a::Role::User, task, vec![a2a::Part::text("hi")])
}

fn agent_message(text: &str) -> a2a::Message {
    a2a::Message::new(a2a::Role::Agent, vec![a2a::Part::text(text)])
}

fn error_text_part(text: &str) -> a2a::Part {
    let mut part = a2a::Part::text(text);
    part.metadata = Some(HashMap::from([(
        crate::convert::METADATA_IS_ERR_MESSAGE_KEY.to_owned(),
        json!(true),
    )]));
    part
}

fn status(task: &TaskInfo, state: a2a::TaskState) -> a2a::StreamResponse {
    a2a::StreamResponse::StatusUpdate(new_status_update(task, state, None))
}

fn artifact(task: &TaskInfo, parts: Vec<a2a::Part>) -> a2a::StreamResponse {
    a2a::StreamResponse::ArtifactUpdate(new_artifact_event(task, parts))
}

fn artifact_append(task: &TaskInfo, parts: Vec<a2a::Part>) -> a2a::StreamResponse {
    a2a::StreamResponse::ArtifactUpdate(new_artifact_update_event(
        task,
        &a2a::new_artifact_id(),
        parts,
    ))
}

/// Go's cmp options for the executor tests: ignore `Message.ID`, `Artifact.ID`,
/// `TaskStatus.Timestamp` and the metadata of status and artifact updates (plus, with
/// `ignore_task_info`, their task and context IDs and the artifact metadata). Compared as JSON
/// (plan D5).
fn normalized(events: &[a2a::StreamResponse], per_event: bool) -> Value {
    fn strip(v: &mut Value) {
        match v {
            Value::Object(m) => {
                m.remove("messageId");
                m.remove("artifactId");
                m.remove("timestamp");
                m.values_mut().for_each(strip);
            }
            Value::Array(items) => items.iter_mut().for_each(strip),
            _ => {}
        }
    }
    let mut v = serde_json::to_value(events).unwrap();
    strip(&mut v);
    for event in v.as_array_mut().unwrap() {
        for key in ["statusUpdate", "artifactUpdate"] {
            if let Some(Value::Object(update)) = event.get_mut(key) {
                update.remove("metadata");
                if per_event {
                    update.remove("taskId");
                    update.remove("contextId");
                    if let Some(Value::Object(artifact)) = update.get_mut("artifact") {
                        artifact.remove("metadata");
                    }
                }
            }
        }
    }
    v
}

/// Collects an execution, stopping at the first error.
async fn collect(
    executor: &Executor,
    ctx: a2a_server::ExecutorContext,
) -> (Vec<a2a::StreamResponse>, Option<a2a::A2AError>) {
    let mut stream = executor.execute(ctx);
    let mut events = Vec::new();
    while let Some(item) = stream.next().await {
        match item {
            Ok(event) => events.push(event),
            Err(err) => return (events, Some(err)),
        }
    }
    (events, None)
}

#[tokio::test]
async fn test_executor_execute() {
    let task = task_info();

    struct Case {
        name: &'static str,
        message: Option<a2a::Message>,
        events: Vec<Event>,
        want_events: Vec<a2a::StreamResponse>,
        create_session_fails: bool,
        agent_run_fails: Option<&'static str>,
        // parity: adapted: Go's failing queue write is the consumer stopping; here the consumer
        // stops reading after this many events (the stream is dropped).
        consumer_stops_after: Option<usize>,
        want_err: bool,
    }
    let hi = hi_msg();
    let malformed = {
        let mut part = a2a::Part::data(json!("not a function call"));
        part.metadata = Some(HashMap::from([(
            DATA_PART_META_TYPE_KEY.to_owned(),
            json!(DATA_PART_TYPE_FUNCTION_CALL),
        )]));
        new_message_for_task(a2a::Role::User, &task, vec![part])
    };
    let text = |t: &str| a2a::Part::text(t);
    let cases = vec![
        Case {
            name: "no message",
            message: None,
            events: vec![],
            want_events: vec![],
            create_session_fails: false,
            agent_run_fails: None,
            consumer_stops_after: None,
            want_err: true,
        },
        Case {
            // parity: adapted: Go marshals a channel to fail; a function-call data part that is
            // not an object fails the Rust conversion.
            name: "malformed data",
            message: Some(malformed),
            events: vec![],
            want_events: vec![],
            create_session_fails: false,
            agent_run_fails: None,
            consumer_stops_after: None,
            want_err: true,
        },
        Case {
            name: "session setup fails",
            message: Some(hi_msg_for_task(&task)),
            events: vec![],
            want_events: vec![new_final_status_update(
                &task,
                a2a::TaskState::Failed,
                Some(new_message_for_task(
                    a2a::Role::Agent,
                    &task,
                    vec![error_text_part(
                        "failed to create a session: session creation failed",
                    )],
                )),
            )],
            create_session_fails: true,
            agent_run_fails: None,
            consumer_stops_after: None,
            want_err: false,
        },
        Case {
            name: "success for a new task",
            message: Some(hi.clone()),
            events: vec![
                response_event(model_response_from_parts(vec![Part::from_text("Hello")])),
                response_event(model_response_from_parts(vec![Part::from_text(", world!")])),
            ],
            want_events: vec![
                // The executor marks every task it emits with the ADK A2A extension key.
                a2a::StreamResponse::Task(a2a::Task {
                    id: task.task_id.clone(),
                    context_id: task.context_id.clone(),
                    status: a2a::TaskStatus {
                        state: a2a::TaskState::Submitted,
                        message: None,
                        timestamp: None,
                    },
                    artifacts: None,
                    history: Some(vec![hi.clone()]),
                    metadata: Some(HashMap::from([(ADK_EXTENSION_URI.to_owned(), json!(true))])),
                }),
                status(&task, a2a::TaskState::Working),
                artifact(&task, vec![text("Hello")]),
                artifact_append(&task, vec![text(", world!")]),
                new_final_status_update(&task, a2a::TaskState::Completed, None),
            ],
            create_session_fails: false,
            agent_run_fails: None,
            consumer_stops_after: None,
            want_err: false,
        },
        Case {
            name: "success for existing task",
            message: Some(hi_msg_for_task(&task)),
            events: vec![
                response_event(model_response_from_parts(vec![Part::from_text("Hello")])),
                response_event(model_response_from_parts(vec![Part::from_text(", world!")])),
            ],
            want_events: vec![
                status(&task, a2a::TaskState::Working),
                artifact(&task, vec![text("Hello")]),
                artifact_append(&task, vec![text(", world!")]),
                new_final_status_update(&task, a2a::TaskState::Completed, None),
            ],
            create_session_fails: false,
            agent_run_fails: None,
            consumer_stops_after: None,
            want_err: false,
        },
        Case {
            name: "queue write fails",
            message: Some(hi_msg_for_task(&task)),
            events: vec![],
            want_events: vec![],
            create_session_fails: false,
            agent_run_fails: None,
            consumer_stops_after: Some(0),
            want_err: false,
        },
        Case {
            name: "llm fails",
            message: Some(hi_msg_for_task(&task)),
            events: vec![
                response_event(model_response_from_parts(vec![Part::from_text("Hello")])),
                response_event(LlmResponse {
                    error_code: "418".into(),
                    error_message: "I'm a teapot".into(),
                    ..Default::default()
                }),
                response_event(model_response_from_parts(vec![Part::from_text(", world!")])),
            ],
            want_events: vec![
                status(&task, a2a::TaskState::Working),
                artifact(&task, vec![text("Hello")]),
                artifact_append(&task, vec![text(", world!")]),
                a2a::StreamResponse::StatusUpdate(to_task_failed_update_event(
                    &task,
                    &error_from_response(&LlmResponse {
                        error_code: "418".into(),
                        error_message: "I'm a teapot".into(),
                        ..Default::default()
                    }),
                    None,
                )),
            ],
            create_session_fails: false,
            agent_run_fails: None,
            consumer_stops_after: None,
            want_err: false,
        },
        Case {
            name: "agent run fails",
            message: Some(hi_msg_for_task(&task)),
            events: vec![response_event(model_response_from_parts(vec![
                Part::from_text("Hello"),
            ]))],
            want_events: vec![
                status(&task, a2a::TaskState::Working),
                artifact(&task, vec![text("Hello")]),
                new_final_status_update(
                    &task,
                    a2a::TaskState::Failed,
                    Some(new_message_for_task(
                        a2a::Role::Agent,
                        &task,
                        vec![error_text_part("agent run failed: OOF")],
                    )),
                ),
            ],
            create_session_fails: false,
            agent_run_fails: Some("OOF"),
            consumer_stops_after: None,
            want_err: false,
        },
        Case {
            name: "agent run and queue write fail",
            message: Some(hi_msg_for_task(&task)),
            events: vec![response_event(model_response_from_parts(vec![
                Part::from_text("Hello"),
            ]))],
            want_events: vec![
                status(&task, a2a::TaskState::Working),
                artifact(&task, vec![text("Hello")]),
            ],
            create_session_fails: false,
            agent_run_fails: Some("OOF"),
            consumer_stops_after: Some(2),
            want_err: false,
        },
    ];

    for tc in cases {
        let agent = new_event_replay_agent(tc.events, tc.agent_run_fails);
        let executor = Executor::new(ExecutorConfig {
            runner_config: Some(runner_config(
                agent,
                session_service(tc.create_session_fails),
            )),
            ..Default::default()
        });
        let stored = tc
            .message
            .as_ref()
            .is_some_and(|m| m.task_id.as_deref() == Some(task.task_id.as_str()));
        let ctx = request(&task, tc.message, stored);

        let (got, err) = match tc.consumer_stops_after {
            Some(n) => {
                let got: Vec<_> = executor.execute(ctx).take(n).collect().await;
                (got.into_iter().map(Result::unwrap).collect(), None)
            }
            None => collect(&executor, ctx).await,
        };
        match (err, tc.want_err) {
            (Some(_), true) => continue,
            (Some(err), false) => panic!("{}: execute() error = {err:?}, want nil", tc.name),
            (None, true) => panic!("{}: execute() error = nil, want error", tc.name),
            (None, false) => {}
        }
        assert_eq!(
            normalized(&got, false),
            normalized(&tc.want_events, false),
            "{}: execute() produced wrong events",
            tc.name
        );
    }
}

#[tokio::test]
async fn test_executor_cancel() {
    let task = task_info();
    let executor = Executor::new(ExecutorConfig::default());
    let ctx = request(&task, None, true);

    let got: Vec<_> = executor.cancel(ctx).collect().await;
    assert_eq!(
        got.len(),
        1,
        "cancel() produced {} events, want 1",
        got.len()
    );
    let Ok(a2a::StreamResponse::StatusUpdate(event)) = &got[0] else {
        panic!("cancel() = {:?}, want a status update", got[0]);
    };
    assert_eq!(event.status.state, a2a::TaskState::Canceled);
}

#[tokio::test]
async fn test_executor_session_reuse() {
    let agent = new_event_replay_agent(vec![], None);
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    let task = task_info();
    let msg = new_message_for_task(a2a::Role::User, &task, vec![]);
    let ctx = || request(&task, Some(msg.clone()), false);
    let config = runner_config(Arc::clone(&agent), Arc::clone(&sessions));
    let executor = Executor::new(ExecutorConfig {
        runner_config: Some(config.clone()),
        ..Default::default()
    });

    for _ in 0..2 {
        let (_, err) = collect(&executor, ctx()).await;
        assert!(err.is_none(), "execute() error = {err:?}, want nil");
    }

    let internal = RunnerConfig {
        app_name: config.app_name.clone(),
        agent: Arc::clone(&config.agent),
        session_service: Arc::clone(&config.session_service),
    };
    let meta = executor.invocation_meta(&internal, &ctx());
    let listed = sessions
        .list(ListRequest {
            app_name: config.app_name.clone(),
            user_id: meta.user_id.clone(),
        })
        .await
        .unwrap();
    assert_eq!(
        listed.len(),
        1,
        "list() got {} sessions, want 1",
        listed.len()
    );

    let mut other = ctx();
    other.context_id = a2a::new_context_id();
    let other_meta = executor.invocation_meta(&internal, &other);
    assert_ne!(
        meta.session_id, other_meta.session_id,
        "want sessionID to be different for different contextIDs"
    );
}

#[tokio::test]
async fn test_executor_callbacks() {
    let task = task_info();

    /// Go's context value (`contextKeyType{}` → `"bar"`), stored in the extensions.
    #[derive(Clone)]
    struct ContextValue(String);

    struct Case {
        name: &'static str,
        create_session_fails: bool,
        events: Vec<Event>,
        before_execution: Option<BeforeExecuteCallback>,
        after_event: Option<AfterEventCallback>,
        after_execution: Option<AfterExecuteCallback>,
        want_events: Option<Vec<a2a::StreamResponse>>,
        want_err: bool,
    }
    let set_message = |text: &'static str| -> AfterExecuteCallback {
        Arc::new(move |_ctx, final_update, _err| {
            final_update.status.message = Some(agent_message(text));
            Box::pin(async { Ok(()) })
        })
    };
    let hello_world = || {
        vec![
            response_event(model_response_from_parts(vec![Part::from_text("Hello")])),
            response_event(model_response_from_parts(vec![Part::from_text(", world!")])),
        ]
    };
    let text = |t: &str| a2a::Part::text(t);
    let cases = vec![
        Case {
            name: "abort execution",
            create_session_fails: false,
            events: vec![],
            before_execution: Some(Arc::new(|_ctx, _ext| {
                Box::pin(async { Err(Error::msg("aborted")) })
            })),
            after_event: None,
            after_execution: None,
            want_events: None,
            want_err: true,
        },
        Case {
            name: "instrument context",
            create_session_fails: false,
            events: vec![],
            before_execution: Some(Arc::new(|_ctx, ext| {
                ext.insert(ContextValue("bar".into()));
                Box::pin(async { Ok(()) })
            })),
            after_event: None,
            after_execution: Some(Arc::new(|ctx, final_update, _err| {
                let text = ctx
                    .extensions()
                    .get::<ContextValue>()
                    .map(|v| v.0.clone())
                    .unwrap_or_default();
                final_update.status.message = Some(agent_message(&text));
                Box::pin(async { Ok(()) })
            })),
            want_events: Some(vec![
                status(&task, a2a::TaskState::Working),
                new_final_status_update(
                    &task,
                    a2a::TaskState::Completed,
                    Some(agent_message("bar")),
                ),
            ]),
            want_err: false,
        },
        Case {
            name: "intercept processing failure",
            create_session_fails: false,
            events: vec![response_event(model_response_from_parts(vec![
                Part::from_text("Hello, world!"),
            ]))],
            before_execution: None,
            after_event: Some(Arc::new(|_ctx, _event, _processed| {
                Box::pin(async { Err(Error::msg("fail!")) })
            })),
            after_execution: Some(set_message("bar")),
            want_events: Some(vec![
                status(&task, a2a::TaskState::Working),
                new_final_status_update(&task, a2a::TaskState::Failed, Some(agent_message("bar"))),
            ]),
            want_err: false,
        },
        Case {
            name: "intercept session setup failure",
            create_session_fails: true,
            events: vec![],
            before_execution: None,
            after_event: None,
            after_execution: Some(Arc::new(|ctx, final_update, _err| {
                let count = ctx.readonly_state().len();
                final_update.status.message = Some(agent_message(&format!("{count} events")));
                Box::pin(async { Ok(()) })
            })),
            want_events: Some(vec![new_final_status_update(
                &task,
                a2a::TaskState::Failed,
                Some(agent_message("0 events")),
            )]),
            want_err: false,
        },
        Case {
            name: "enrich event",
            create_session_fails: false,
            events: hello_world(),
            before_execution: None,
            after_event: Some(Arc::new(|_ctx, _event, processed| {
                processed
                    .artifact
                    .parts
                    .push(a2a::Part::text(" (enriched)"));
                Box::pin(async { Ok(()) })
            })),
            after_execution: None,
            want_events: Some(vec![
                status(&task, a2a::TaskState::Working),
                artifact(&task, vec![text("Hello"), text(" (enriched)")]),
                artifact_append(&task, vec![text(", world!"), text(" (enriched)")]),
                new_final_status_update(&task, a2a::TaskState::Completed, None),
            ]),
            want_err: false,
        },
        Case {
            name: "can access session events",
            create_session_fails: false,
            events: hello_world(),
            before_execution: None,
            after_event: None,
            after_execution: Some(Arc::new(|ctx, final_update, _err| {
                let count = ctx.events().len();
                final_update.status.message =
                    Some(agent_message(&format!("event count = {count}")));
                Box::pin(async { Ok(()) })
            })),
            want_events: Some(vec![
                status(&task, a2a::TaskState::Working),
                artifact(&task, vec![text("Hello")]),
                artifact_append(&task, vec![text(", world!")]),
                new_final_status_update(
                    &task,
                    a2a::TaskState::Completed,
                    Some(agent_message("event count = 3")),
                ),
            ]),
            want_err: false,
        },
        Case {
            name: "abort execution",
            create_session_fails: false,
            events: hello_world(),
            before_execution: None,
            after_event: Some(Arc::new(|_ctx, _event, _processed| {
                Box::pin(async { Err(Error::msg("abort execution")) })
            })),
            after_execution: None,
            want_events: Some(vec![
                status(&task, a2a::TaskState::Working),
                a2a::StreamResponse::StatusUpdate(to_task_failed_update_event(
                    &task,
                    "processor failed: abort execution",
                    None,
                )),
            ]),
            want_err: false,
        },
    ];

    for tc in cases {
        let agent = new_event_replay_agent(tc.events, None);
        let executor = Executor::new(ExecutorConfig {
            runner_config: Some(runner_config(
                agent,
                session_service(tc.create_session_fails),
            )),
            before_execute_callback: tc.before_execution,
            after_event_callback: tc.after_event,
            after_execute_callback: tc.after_execution,
            ..Default::default()
        });
        let (got, err) = collect(
            &executor,
            request(&task, Some(hi_msg_for_task(&task)), true),
        )
        .await;
        match (err, tc.want_err) {
            (Some(_), true) => continue,
            (Some(err), false) => panic!("{}: execute() error = {err:?}, want nil", tc.name),
            (None, true) => panic!("{}: execute() error = nil, want error", tc.name),
            (None, false) => {}
        }
        if let Some(want) = tc.want_events {
            assert_eq!(
                normalized(&got, false),
                normalized(&want, false),
                "{}: execute() produced wrong events",
                tc.name
            );
        }
    }
}

/// Signals when dropped.
struct DropSignal(Option<tokio::sync::oneshot::Sender<()>>);

impl Drop for DropSignal {
    fn drop(&mut self) {
        if let Some(tx) = self.0.take() {
            let _ = tx.send(());
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
// parity: adapted: a2a-rs does not interrupt a running execution on cancel (Go cancels its context); it drops the execute stream at the next event after the task was canceled, so the agent here yields an event every 20 ms and must be dropped within 1 s
async fn test_executor_cancel_after_event() {
    let (unblocked_tx, unblocked_rx) = tokio::sync::oneshot::channel::<()>();
    let unblocked_tx = Arc::new(Mutex::new(Some(unblocked_tx)));
    let agent = custom_agent(move |ctx: InvocationContext| {
        let guard = DropSignal(unblocked_tx.lock().take());
        let token = ctx.cancellation_token().clone();
        Box::pin(async_stream::stream! {
            let _guard = guard;
            loop {
                tokio::select! {
                    () = token.cancelled() => {
                        yield Err(AgentError::other("context canceled"));
                        return;
                    }
                    () = tokio::time::sleep(Duration::from_millis(20)) => {
                        yield Ok(response_event(model_response_from_parts(vec![Part::from_text("tick")])));
                    }
                }
            }
        })
    });
    let executor = Executor::new(ExecutorConfig {
        runner_config: Some(runner_config(
            agent,
            Arc::new(InMemorySessionService::new()),
        )),
        ..Default::default()
    });
    let card: a2a::AgentCard = serde_json::from_value(json!({
        "name": "test", "description": "", "version": "1.0.0",
        "supportedInterfaces": [], "capabilities": {},
        "defaultInputModes": [], "defaultOutputModes": [], "skills": [],
    }))
    .unwrap();
    let server = spawn_a2a_server(executor, card).await;
    let client = new_a2a_client_provider(None)
        .create_client(ClientContext::Detached, &server.card)
        .await
        .unwrap();

    let req: a2a::SendMessageRequest = serde_json::from_value(json!({
        "message": {"messageId": a2a::new_message_id(), "role": "ROLE_USER", "parts": [{"text": "TEST"}]},
        "configuration": {"returnImmediately": true},
    }))
    .unwrap();
    let result = client.send_message(&req).await.unwrap();
    let a2a::SendMessageResponse::Task(task) = result else {
        panic!("send_message() = {result:?}, want a task");
    };

    let canceled = client
        .cancel_task(&a2a::CancelTaskRequest {
            id: task.id.clone(),
            metadata: None,
            tenant: None,
        })
        .await
        .unwrap();
    assert_eq!(canceled.status.state, a2a::TaskState::Canceled);

    // Verify that the execution is stopped.
    tokio::time::timeout(Duration::from_secs(1), unblocked_rx)
        .await
        .expect("Agent did not unblock")
        .unwrap();
}

#[tokio::test]
async fn test_executor_converters() {
    let task = task_info();
    let ctx = || request(&task, Some(hi_msg_for_task(&task)), true);

    // A2APartConverter: modify input
    {
        let received = Arc::new(Mutex::new(String::new()));
        let seen = Arc::clone(&received);
        let agent = custom_agent(move |ctx: InvocationContext| {
            if let Some(part) = ctx.user_content().and_then(|c| c.parts.first()) {
                *seen.lock() = part.text.clone();
            }
            futures::stream::empty().boxed()
        });
        let executor = Executor::new(ExecutorConfig {
            runner_config: Some(runner_config(
                agent,
                Arc::new(InMemorySessionService::new()),
            )),
            a2a_part_converter: Some(Arc::new(|_evt: StreamResponseRef<'_>, part: &a2a::Part| {
                let out = (part.as_text() == Some("hi")).then(|| Part::from_text("HELLO"));
                Box::pin(async move { Ok(out) })
            })),
            ..Default::default()
        });
        let (_, err) = collect(&executor, ctx()).await;
        assert!(err.is_none(), "execute() error = {err:?}");
        assert_eq!(*received.lock(), "HELLO", "received text");
    }

    // A2APartConverter: filter input
    {
        let received = Arc::new(Mutex::new(usize::MAX));
        let seen = Arc::clone(&received);
        let agent = custom_agent(move |ctx: InvocationContext| {
            *seen.lock() = ctx.user_content().map_or(0, |c| c.parts.len());
            futures::stream::empty().boxed()
        });
        let executor = Executor::new(ExecutorConfig {
            runner_config: Some(runner_config(
                agent,
                Arc::new(InMemorySessionService::new()),
            )),
            a2a_part_converter: Some(Arc::new(
                |_evt: StreamResponseRef<'_>, _part: &a2a::Part| Box::pin(async { Ok(None) }),
            )),
            ..Default::default()
        });
        let (_, err) = collect(&executor, ctx()).await;
        assert!(err.is_none(), "execute() error = {err:?}");
        assert_eq!(*received.lock(), 0, "received parts count");
    }

    let agent_events = || {
        vec![response_event(LlmResponse {
            content: Some(Content {
                parts: vec![Part::from_text("world")],
                role: String::new(),
            }),
            ..Default::default()
        })]
    };

    // GenAIPartConverter: modify output
    {
        let executor = Executor::new(ExecutorConfig {
            runner_config: Some(runner_config(
                new_event_replay_agent(agent_events(), None),
                Arc::new(InMemorySessionService::new()),
            )),
            genai_part_converter: Some(Arc::new(|_evt: Option<&Event>, part: &Part| {
                let text = if part.text == "world" {
                    "WORLD".to_owned()
                } else {
                    part.text.clone()
                };
                Box::pin(async move { Ok(Some(a2a::Part::text(text))) })
            })),
            ..Default::default()
        });
        let (got, err) = collect(&executor, ctx()).await;
        assert!(err.is_none(), "execute() error = {err:?}");
        let found = got.iter().any(|e| {
            matches!(e, a2a::StreamResponse::ArtifactUpdate(u)
                if u.artifact.parts.iter().any(|p| p.as_text() == Some("WORLD")))
        });
        assert!(found, "did not find 'WORLD' in events: {got:?}");
    }

    // GenAIPartConverter: filter output
    {
        let executor = Executor::new(ExecutorConfig {
            runner_config: Some(runner_config(
                new_event_replay_agent(agent_events(), None),
                Arc::new(InMemorySessionService::new()),
            )),
            genai_part_converter: Some(Arc::new(|_evt: Option<&Event>, _part: &Part| {
                Box::pin(async { Ok(None) })
            })),
            ..Default::default()
        });
        let (got, err) = collect(&executor, ctx()).await;
        assert!(err.is_none(), "execute() error = {err:?}");
        for e in &got {
            if let a2a::StreamResponse::ArtifactUpdate(u) = e {
                assert!(
                    u.artifact.parts.is_empty(),
                    "found {} parts but expected 0",
                    u.artifact.parts.len()
                );
            }
        }
    }
}

#[tokio::test]
async fn test_executor_output_artifact_per_event() {
    let task = task_info();
    let update = |parts: Vec<a2a::Part>, append: bool, last_chunk: bool| {
        a2a::StreamResponse::ArtifactUpdate(a2a::TaskArtifactUpdateEvent {
            task_id: String::new(),
            context_id: String::new(),
            artifact: a2a::Artifact {
                artifact_id: String::new(),
                name: None,
                description: None,
                parts,
                metadata: None,
                extensions: None,
            },
            append: append.then_some(true),
            last_chunk: last_chunk.then_some(true),
            metadata: None,
        })
    };
    let text = |t: &str| a2a::Part::text(t);
    let thought = |t: &str| {
        let mut p = a2a::Part::text(t);
        p.metadata = Some(HashMap::from([(to_a2a_meta_key("thought"), json!(true))]));
        p
    };
    let working = || status(&task, a2a::TaskState::Working);
    let completed = || new_final_status_update(&task, a2a::TaskState::Completed, None);

    struct Case {
        name: &'static str,
        events: Vec<Event>,
        want_events: Vec<a2a::StreamResponse>,
        want_artifact_meta: Vec<Value>,
    }
    let cases = vec![
        Case {
            name: "single artifact per event chain",
            events: vec![
                authored(
                    "agent",
                    model_partial_response_from_parts(vec![Part::from_text("Hello, ")]),
                ),
                authored(
                    "agent",
                    model_partial_response_from_parts(vec![Part::from_text("world!")]),
                ),
                authored(
                    "agent",
                    model_response_from_parts(vec![Part::from_text("Hello, world!")]),
                ),
            ],
            want_events: vec![
                working(),
                update(vec![text("Hello, ")], false, false),
                update(vec![text("world!")], true, false),
                update(vec![text("Hello, world!")], false, true),
                completed(),
            ],
            want_artifact_meta: vec![],
        },
        Case {
            name: "multiple authors",
            events: vec![
                authored(
                    "agent1",
                    model_partial_response_from_parts(vec![Part::from_text("Agent1: H")]),
                ),
                authored(
                    "agent2",
                    model_partial_response_from_parts(vec![Part::from_text("Agent2: W")]),
                ),
                authored(
                    "agent1",
                    model_response_from_parts(vec![Part::from_text("Agent1: Hello")]),
                ),
                authored(
                    "agent2",
                    model_response_from_parts(vec![Part::from_text("Agent2: World")]),
                ),
            ],
            want_events: vec![
                working(),
                update(vec![text("Agent1: H")], false, false),
                update(vec![text("Agent2: W")], false, false),
                update(vec![text("Agent1: Hello")], false, true),
                update(vec![text("Agent2: World")], false, true),
                completed(),
            ],
            want_artifact_meta: vec![],
        },
        Case {
            name: "metadata and thoughts",
            events: vec![
                Event {
                    invocation_id: "inv1".into(),
                    ..authored(
                        "agent",
                        model_partial_response_from_parts(vec![Part {
                            text: "Thinking...".into(),
                            thought: true,
                            ..Default::default()
                        }]),
                    )
                },
                Event {
                    invocation_id: "inv1".into(),
                    ..authored(
                        "agent",
                        model_response_from_parts(vec![Part::from_text("Done")]),
                    )
                },
            ],
            want_events: vec![
                working(),
                update(vec![thought("Thinking...")], false, false),
                update(vec![text("Done")], false, true),
                completed(),
            ],
            want_artifact_meta: vec![
                json!({ to_a2a_meta_key("invocation_id"): "inv1" }),
                json!({ to_a2a_meta_key("invocation_id"): "inv1" }),
            ],
        },
        Case {
            name: "mixed segments",
            events: vec![authored(
                "agent",
                model_response_from_parts(vec![
                    Part {
                        text: "Thought part".into(),
                        thought: true,
                        ..Default::default()
                    },
                    Part::from_text("Text part"),
                ]),
            )],
            want_events: vec![
                working(),
                update(
                    vec![thought("Thought part"), text("Text part")],
                    false,
                    true,
                ),
                completed(),
            ],
            want_artifact_meta: vec![],
        },
        Case {
            name: "sequential distinct artifact chains",
            events: vec![
                authored(
                    "agent",
                    model_partial_response_from_parts(vec![Part::from_text("1.a")]),
                ),
                authored(
                    "agent",
                    model_response_from_parts(vec![Part::from_text("1.final")]),
                ),
                authored(
                    "agent",
                    model_partial_response_from_parts(vec![Part::from_text("2.a")]),
                ),
                authored(
                    "agent",
                    model_response_from_parts(vec![Part::from_text("2.final")]),
                ),
            ],
            want_events: vec![
                working(),
                update(vec![text("1.a")], false, false),
                update(vec![text("1.final")], false, true),
                update(vec![text("2.a")], false, false),
                update(vec![text("2.final")], false, true),
                completed(),
            ],
            want_artifact_meta: vec![],
        },
    ];

    for tc in cases {
        let executor = Executor::new(ExecutorConfig {
            runner_config: Some(runner_config(
                new_event_replay_agent(tc.events.clone(), None),
                Arc::new(InMemorySessionService::new()),
            )),
            output_mode: OutputMode::ArtifactPerEvent,
            ..Default::default()
        });
        let (got, err) = collect(
            &executor,
            request(&task, Some(hi_msg_for_task(&task)), true),
        )
        .await;
        assert!(err.is_none(), "{}: execute() error = {err:?}", tc.name);
        assert_eq!(
            got.len(),
            tc.want_events.len(),
            "{}: got {} events, want {}",
            tc.name,
            got.len(),
            tc.want_events.len()
        );
        assert_eq!(
            normalized(&got, true),
            normalized(&tc.want_events, true),
            "{}: events mismatch",
            tc.name
        );

        let mut author_to_id: HashMap<String, String> = HashMap::new();
        let mut last_finished_id: HashMap<String, String> = HashMap::new();
        let mut artifact_count = 0;
        for (i, got) in got.iter().enumerate() {
            let a2a::StreamResponse::ArtifactUpdate(ge) = got else {
                continue;
            };
            // Metadata check for cases that care about it
            if let Some(want_meta) = tc.want_artifact_meta.get(artifact_count) {
                for (k, v) in want_meta.as_object().unwrap() {
                    let got_v = ge.artifact.metadata.as_ref().and_then(|m| m.get(k));
                    assert_eq!(got_v, Some(v), "{}: event[{i}] Metadata[{k}]", tc.name);
                }
            }
            artifact_count += 1;

            // Custom check for ArtifactID consistency
            let source = &tc.events[i - 1]; // i-1 because the first event is the working status
            let author = &source.author;
            match author_to_id.get(author) {
                Some(id) => assert_eq!(
                    &ge.artifact.artifact_id, id,
                    "{}: event[{i}] expected ArtifactID",
                    tc.name
                ),
                None => {
                    // New artifact stream started for this author
                    if let Some(prev) = last_finished_id.get(author) {
                        assert_ne!(
                            &ge.artifact.artifact_id, prev,
                            "{}: event[{i}] expected NEW ArtifactID, but got same as previous chain",
                            tc.name
                        );
                    }
                    author_to_id.insert(author.clone(), ge.artifact.artifact_id.clone());
                }
            }
            if !source.response.partial {
                // Stream ended for this author
                last_finished_id.insert(author.clone(), ge.artifact.artifact_id.clone());
                author_to_id.remove(author);
            }
        }
    }
}

#[tokio::test]
// parity: adapted: Go's testRunner is the crate's ScriptedRunner
async fn test_executor_runner_provider() {
    let want_text = "Hello";
    let task = task_info();
    let agent = gadk_agent::new_agent(AgentConfig {
        name: "agent".into(),
        ..Default::default()
    })
    .unwrap();
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    let runner = Arc::new(ScriptedRunner::new(vec![Ok(response_event(
        model_response_from_parts(vec![Part::from_text(want_text)]),
    ))]));
    let executor = Executor::new(ExecutorConfig {
        runner_config: Some(runner_config(Arc::clone(&agent), Arc::clone(&sessions))),
        runner_provider: Some(Arc::new(move |_ctx, _plugin| {
            let cfg = RunnerConfig {
                app_name: "test".into(),
                agent: Arc::clone(&agent),
                session_service: Arc::clone(&sessions),
            };
            let runner = Arc::clone(&runner) as Arc<dyn crate::server::Runner>;
            Box::pin(async move { Ok((cfg, runner)) })
        })),
        ..Default::default()
    });

    let (events, err) = collect(
        &executor,
        request(&task, Some(hi_msg_for_task(&task)), true),
    )
    .await;
    assert!(err.is_none(), "execute() error = {err:?}");
    assert!(
        events.len() >= 2,
        "execute() produced {} events, want at least 2",
        events.len()
    );
    let a2a::StreamResponse::ArtifactUpdate(ta) = &events[1] else {
        panic!("events[1] = {:?}, want an artifact update", events[1]);
    };
    assert_eq!(
        ta.artifact.parts[0].as_text(),
        Some(want_text),
        "ta.Artifact.Parts[0] = {:?}",
        ta.artifact.parts[0]
    );
}
