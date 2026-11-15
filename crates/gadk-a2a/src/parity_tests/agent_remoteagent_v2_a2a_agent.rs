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

//! Ports of ADK-Go `agent/remoteagent/v2/a2a_agent_test.go`. [`harness`] holds the Go file's
//! test helpers (`startA2AServer`, `newA2AEventReplay`, `newInvocationContext`, `runAndCollect`,
//! …), shared with the other remote-agent targets.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

/// The Go file's helpers. Go serves the remote agent with `httptest` + `a2asrv`; here an a2a-rs
/// `DefaultRequestHandler` + `InMemoryTaskStore` behind the JSON-RPC router on an ephemeral
/// axum listener (feature `server`).
#[cfg_attr(not(feature = "server"), allow(dead_code))]
pub(crate) mod harness {
    use std::sync::Arc;

    use futures::StreamExt as _;
    use futures::stream::BoxStream;
    use gadk_agent::{Agent, AgentError, InvocationContext, RunConfig, StreamingMode};
    use gadk_genai::{Content, ROLE_USER};
    use gadk_platform::Platform;
    use gadk_session::{CreateRequest, Event, InMemorySessionService, SessionService};

    use crate::remoteagent::{A2AConfig, new_a2a};

    /// The executor's stream type.
    pub(crate) type A2AStream = BoxStream<'static, Result<a2a::StreamResponse, a2a::A2AError>>;
    #[cfg(feature = "server")]
    /// A scripted `execute`/`cancel`.
    pub(crate) type ExecFn =
        Arc<dyn Fn(a2a_server::ExecutorContext) -> A2AStream + Send + Sync + 'static>;

    #[cfg(feature = "server")]
    /// Go `mockA2AExecutor`: `execute`/`cancel` run the scripted functions; without one, Go's
    /// `not implemented` error.
    #[derive(Clone, Default)]
    pub(crate) struct MockExecutor {
        pub(crate) execute: Option<ExecFn>,
        pub(crate) cancel: Option<ExecFn>,
    }

    #[cfg(feature = "server")]
    impl a2a_server::AgentExecutor for MockExecutor {
        fn execute(&self, ctx: a2a_server::ExecutorContext) -> A2AStream {
            match &self.execute {
                Some(f) => f(ctx),
                None => {
                    futures::stream::once(async { Err(a2a::A2AError::internal("not implemented")) })
                        .boxed()
                }
            }
        }

        fn cancel(&self, ctx: a2a_server::ExecutorContext) -> A2AStream {
            match &self.cancel {
                Some(f) => f(ctx),
                None => {
                    let update =
                        status_update(&ctx.task_id, &ctx.context_id, a2a::TaskState::Canceled);
                    futures::stream::iter(vec![Ok(a2a::StreamResponse::StatusUpdate(update))])
                        .boxed()
                }
            }
        }
    }

    /// Go `a2a.NewStatusUpdateEvent(task, state, nil)`.
    pub(crate) fn status_update(
        task_id: &str,
        context_id: &str,
        state: a2a::TaskState,
    ) -> a2a::TaskStatusUpdateEvent {
        a2a::TaskStatusUpdateEvent {
            task_id: task_id.to_owned(),
            context_id: context_id.to_owned(),
            status: a2a::TaskStatus {
                state,
                message: None,
                timestamp: None,
            },
            metadata: None,
        }
    }

    #[cfg(feature = "server")]
    /// Go `a2a.NewSubmittedTask(execCtx, execCtx.Message)`.
    pub(crate) fn submitted_task(ctx: &a2a_server::ExecutorContext) -> a2a::Task {
        a2a::Task {
            id: ctx.task_id.clone(),
            context_id: ctx.context_id.clone(),
            status: a2a::TaskStatus {
                state: a2a::TaskState::Submitted,
                message: None,
                timestamp: None,
            },
            artifacts: None,
            history: ctx.message.clone().map(|m| vec![m]),
            metadata: None,
        }
    }

    #[cfg(feature = "server")]
    /// Go `newA2AEventReplay`: yields a submitted task first (unless the script starts with a
    /// task or a message), then the scripted events with the request's task and context IDs.
    pub(crate) fn a2a_event_replay(events: Vec<a2a::StreamResponse>) -> MockExecutor {
        let events = Arc::new(events);
        MockExecutor {
            execute: Some(Arc::new(move |ctx| {
                let mut out = Vec::new();
                if let Some(first) = events.first()
                    && !matches!(
                        first,
                        a2a::StreamResponse::Task(_) | a2a::StreamResponse::Message(_)
                    )
                {
                    out.push(Ok(a2a::StreamResponse::Task(submitted_task(&ctx))));
                }
                for ev in events.iter() {
                    // The A2A stack fails the request if events don't have the right IDs.
                    let mut ev = ev.clone();
                    match &mut ev {
                        a2a::StreamResponse::Message(m) => {
                            m.task_id = Some(ctx.task_id.clone());
                            m.context_id = Some(ctx.context_id.clone());
                        }
                        a2a::StreamResponse::Task(t) => {
                            t.id = ctx.task_id.clone();
                            t.context_id = ctx.context_id.clone();
                        }
                        a2a::StreamResponse::StatusUpdate(u) => {
                            u.task_id = ctx.task_id.clone();
                            u.context_id = ctx.context_id.clone();
                        }
                        a2a::StreamResponse::ArtifactUpdate(u) => {
                            u.task_id = ctx.task_id.clone();
                            u.context_id = ctx.context_id.clone();
                        }
                    }
                    out.push(Ok(ev));
                }
                futures::stream::iter(out).boxed()
            })),
            cancel: None,
        }
    }

    /// Go `testA2AServer`: an in-process A2A server; shut down on drop.
    #[cfg(feature = "server")]
    pub(crate) struct TestA2AServer {
        /// The JSON-RPC endpoint (Go `server.URL`).
        pub(crate) url: String,
        shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    }

    #[cfg(feature = "server")]
    impl Drop for TestA2AServer {
        fn drop(&mut self) {
            if let Some(tx) = self.shutdown.take() {
                let _ = tx.send(());
            }
        }
    }

    /// Go `startA2AServer`: the executor behind a2a-rs's request handler and JSON-RPC router.
    #[cfg(feature = "server")]
    pub(crate) async fn start_a2a_server(
        executor: impl a2a_server::AgentExecutor,
    ) -> TestA2AServer {
        let handler = Arc::new(a2a_server::DefaultRequestHandler::new(
            executor,
            a2a_server::InMemoryTaskStore::new(),
        ));
        serve_router(a2a_server::jsonrpc::jsonrpc_router(handler)).await
    }

    /// Serves `router` on an ephemeral loopback port.
    #[cfg(feature = "server")]
    pub(crate) async fn serve_router(router: axum::Router) -> TestA2AServer {
        let (listener, _) = bind_loopback().await;
        serve_router_at(listener, router)
    }

    /// An ephemeral loopback listener and its base URL (Go `httptest.NewUnstartedServer`), for
    /// a server whose routes need their own address.
    #[cfg(feature = "server")]
    pub(crate) async fn bind_loopback() -> (tokio::net::TcpListener, String) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        (listener, base)
    }

    /// Serves `router` on `listener`.
    #[cfg(feature = "server")]
    pub(crate) fn serve_router_at(
        listener: tokio::net::TcpListener,
        router: axum::Router,
    ) -> TestA2AServer {
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        tokio::spawn(async move {
            let _ = axum::serve(listener, router)
                .with_graceful_shutdown(async {
                    let _ = rx.await;
                })
                .await;
        });
        TestA2AServer {
            url: format!("http://{addr}"),
            shutdown: Some(tx),
        }
    }

    /// A card with one JSON-RPC interface at `url` (Go
    /// `a2a.NewAgentInterface(url, a2a.TransportProtocolJSONRPC)`), streaming capable.
    pub(crate) fn jsonrpc_card(url: &str) -> a2a::AgentCard {
        serde_json::from_value(serde_json::json!({
            "name": "",
            "description": "",
            "version": "",
            "supportedInterfaces": [
                {"url": url, "protocolBinding": "JSONRPC", "protocolVersion": "1.0"}
            ],
            "capabilities": {"streaming": true},
            "defaultInputModes": [],
            "defaultOutputModes": [],
            "skills": [],
        }))
        .unwrap()
    }

    /// Go `newA2ARemoteAgent`.
    pub(crate) fn new_a2a_remote_agent(name: &str, url: &str) -> Arc<dyn Agent> {
        new_a2a(A2AConfig {
            name: name.to_owned(),
            agent_card: Some(jsonrpc_card(url)),
            ..Default::default()
        })
        .unwrap()
    }

    /// Go `prepareSession`: an in-memory session (`app_name`, user `test`) holding `events`.
    pub(crate) async fn prepare_session(
        app_name: &str,
        events: Vec<Event>,
    ) -> (Arc<InMemorySessionService>, Arc<dyn gadk_session::Session>) {
        let service = Arc::new(InMemorySessionService::new());
        let session = service
            .create(CreateRequest {
                app_name: app_name.to_owned(),
                user_id: "test".to_owned(),
                ..Default::default()
            })
            .await
            .unwrap();
        for mut event in events {
            service.append_event(&*session, &mut event).await.unwrap();
        }
        let session = service
            .get(gadk_session::GetRequest {
                app_name: app_name.to_owned(),
                user_id: "test".to_owned(),
                session_id: session.id().to_owned(),
                ..Default::default()
            })
            .await
            .unwrap();
        (service, session)
    }

    /// Go `newInvocationContextWithStreamingMode`; `agent` is the agent the context names (the
    /// agent's `run` replaces it with itself).
    pub(crate) async fn invocation_context(
        app_name: &str,
        agent: Arc<dyn Agent>,
        events: Vec<Event>,
        streaming_mode: StreamingMode,
    ) -> InvocationContext {
        let (_, session) = prepare_session(app_name, events).await;
        InvocationContext::builder()
            .session(session)
            .agent(agent)
            .run_config(RunConfig {
                streaming_mode,
                ..Default::default()
            })
            .build_invocation()
    }

    /// Go `runAndCollect`: the events up to the first error, and that error.
    pub(crate) async fn run_and_collect(
        ic: InvocationContext,
        agent: &Arc<dyn Agent>,
    ) -> (Vec<Event>, Option<AgentError>) {
        let mut collected = Vec::new();
        let mut stream = agent.run(ic);
        while let Some(item) = stream.next().await {
            match item {
                Ok(event) => collected.push(event),
                Err(err) => return (collected, Some(err)),
            }
        }
        (collected, None)
    }

    /// Go `newTestInvocationContext` (utils_test.go): a session of app `test`, user
    /// `test-user`, holding `events`, for a plain agent named `agent_name`.
    pub(crate) async fn test_invocation_context(
        agent_name: &str,
        events: Vec<Event>,
    ) -> InvocationContext {
        scoped_context_for("test", "test-user", agent_name, "", None, events).await
    }

    /// Go `newScopedCtx`: a session of app `test`, user `u`, holding `events`, for a plain agent
    /// named `agent_name` running in `scope` with `user_content`.
    pub(crate) async fn scoped_context(
        agent_name: &str,
        scope: &str,
        user_content: Option<Content>,
        events: Vec<Event>,
    ) -> InvocationContext {
        scoped_context_for("test", "u", agent_name, scope, user_content, events).await
    }

    /// The invocation context the Go helpers build with `icontext.NewInvocationContext`.
    pub(crate) async fn scoped_context_for(
        app_name: &str,
        user_id: &str,
        agent_name: &str,
        scope: &str,
        user_content: Option<Content>,
        events: Vec<Event>,
    ) -> InvocationContext {
        let service = InMemorySessionService::new();
        let session = service
            .create(CreateRequest {
                app_name: app_name.to_owned(),
                user_id: user_id.to_owned(),
                ..Default::default()
            })
            .await
            .unwrap();
        for mut event in events {
            service.append_event(&*session, &mut event).await.unwrap();
        }
        let session = service
            .get(gadk_session::GetRequest {
                app_name: app_name.to_owned(),
                user_id: user_id.to_owned(),
                session_id: session.id().to_owned(),
                ..Default::default()
            })
            .await
            .unwrap();
        let agent = gadk_agent::new_agent(gadk_agent::AgentConfig {
            name: agent_name.to_owned(),
            ..Default::default()
        })
        .unwrap();
        let mut builder = InvocationContext::builder()
            .session(session)
            .agent(agent)
            .isolation_scope(scope);
        if let Some(content) = user_content {
            builder = builder.user_content(content);
        }
        builder.build_invocation()
    }

    /// Go `newEventFromParts` (utils_test.go): an event by `author` (role `user` for the user,
    /// else `model`) with empty state and artifact deltas; no content without parts.
    pub(crate) fn event_from_parts(author: &str, parts: Vec<gadk_genai::Part>) -> Event {
        let role = if author == "user" {
            ROLE_USER
        } else {
            gadk_genai::ROLE_MODEL
        };
        let mut event = Event::new(&Platform::default(), "");
        event.author = author.to_owned();
        if !parts.is_empty() {
            event.response.content = Some(Content::from_parts(parts, role));
        }
        event
    }

    /// Go `toLLMResponses`.
    pub(crate) fn to_llm_responses(events: &[Event]) -> Vec<gadk_model::LlmResponse> {
        events.iter().map(|e| e.response.clone()).collect()
    }

    /// `responses` with `custom_metadata` cleared (Go
    /// `cmpopts.IgnoreFields(model.LLMResponse{}, "CustomMetadata")`).
    pub(crate) fn without_custom_metadata(
        mut responses: Vec<gadk_model::LlmResponse>,
    ) -> Vec<gadk_model::LlmResponse> {
        for r in &mut responses {
            r.custom_metadata.clear();
        }
        responses
    }

    /// An artifact update of artifact `id` holding `parts` (Go `a2a.NewArtifactEvent` has
    /// `append: false`, `a2a.NewArtifactUpdateEvent` `append: true`).
    pub(crate) fn artifact_update(
        task_id: &str,
        context_id: &str,
        id: &str,
        parts: Vec<a2a::Part>,
        append: bool,
        last_chunk: bool,
    ) -> a2a::TaskArtifactUpdateEvent {
        a2a::TaskArtifactUpdateEvent {
            task_id: task_id.to_owned(),
            context_id: context_id.to_owned(),
            artifact: a2a::Artifact {
                artifact_id: id.to_owned(),
                name: None,
                description: None,
                parts,
                metadata: None,
                extensions: None,
            },
            append: append.then_some(true),
            last_chunk: last_chunk.then_some(true),
            metadata: None,
        }
    }

    /// Go `newUserHello`.
    pub(crate) fn user_hello() -> Event {
        let mut event = Event::new(&Platform::default(), "invocation");
        event.author = "user".to_owned();
        event.response.content = Some(Content::from_text("hello", ROLE_USER));
        event
    }
}

#[cfg(all(test, feature = "server"))]
mod harness_smoke {
    use gadk_agent::StreamingMode;

    use super::harness::*;

    /// The harness serves a scripted remote agent the remote agent can reach.
    #[tokio::test]
    async fn harness_round_trip() {
        let msg = a2a::Message::new(a2a::Role::Agent, vec![a2a::Part::text("hi")]);
        let server =
            start_a2a_server(a2a_event_replay(vec![a2a::StreamResponse::Message(msg)])).await;
        let agent = new_a2a_remote_agent("a2a", &server.url);
        for mode in [StreamingMode::None, StreamingMode::Sse] {
            let ic = invocation_context("smoke", agent.clone(), vec![user_hello()], mode).await;
            let (events, err) = run_and_collect(ic, &agent).await;
            assert!(err.is_none(), "{err:?}");
            assert_eq!(events.len(), 1, "{events:?}");
            let content = events[0].response.content.as_ref().unwrap();
            assert_eq!(content.parts[0].text, "hi");
        }
    }
}

/// Tests of `newMessage`, `convertParts` and the card provider: no A2A server needed.
mod message_and_card {
    use std::sync::Arc;

    use gadk_genai::{Content, FunctionCall, FunctionResponse, Part, ROLE_USER};
    use serde_json::{Map, json};

    use super::harness::*;
    use crate::convert::{GenAIPartConverter, to_a2a_meta_key, to_custom_metadata};
    use crate::remoteagent::card::tests::serve;
    use crate::remoteagent::card::{
        UNSUPPORTED_CARD_SOURCE, UNTRUSTED_CARD_INTERFACE, classify_card_source, is_loopback_host,
        validate_card_interface_origins,
    };
    use crate::remoteagent::history::convert_parts;
    use crate::remoteagent::{A2AConfig, CardResolveOptions, new_agent_card_provider, new_message};

    fn obj(v: serde_json::Value) -> Map<String, serde_json::Value> {
        match v {
            serde_json::Value::Object(m) => m,
            other => panic!("not an object: {other}"),
        }
    }

    fn texts(msg: &a2a::Message) -> Vec<String> {
        msg.parts
            .iter()
            .map(|p| p.as_text().unwrap_or_default().to_owned())
            .collect()
    }

    fn function_call(id: &str, name: &str) -> Part {
        Part {
            function_call: Some(FunctionCall {
                id: id.into(),
                name: name.into(),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn function_response(id: &str, name: &str, response: serde_json::Value) -> Part {
        Part {
            function_response: Some(FunctionResponse {
                id: id.into(),
                name: name.into(),
                response: obj(response),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn test_remote_agent_custom_converters() {
        let custom: GenAIPartConverter =
            Arc::new(|_event, _part| Box::pin(async { Ok(Some(a2a::Part::text("modified"))) }));
        let cases = [
            (
                "custom converter",
                A2AConfig {
                    genai_part_converter: Some(custom),
                    ..Default::default()
                },
                "modified",
            ),
            ("default converter", A2AConfig::default(), "hello"),
        ];
        for (name, cfg, want) in cases {
            let ictx = test_invocation_context("a2a agent", vec![user_hello()]).await;
            let msg = new_message(&ictx, &cfg).await.unwrap();
            assert_eq!(msg.parts.len(), 1, "{name}");
            assert_eq!(msg.parts[0].as_text(), Some(want), "{name}");
        }
    }

    #[tokio::test]
    async fn test_remote_agent_scoped_message_uses_seeded_input() {
        let scope = "workflow/node@1";
        let mut remote_response =
            event_from_parts("remote-agent", vec![Part::from_text("sibling response")]);
        remote_response.isolation_scope = "workflow/other@1".into();
        remote_response.response.custom_metadata =
            to_custom_metadata(&a2a::new_task_id(), "context-from-sibling");
        let shared_history = event_from_parts("user", vec![Part::from_text("shared history")]);

        let invocation = scoped_context_for(
            "TestRemoteAgent_ScopedMessageUsesSeededInput",
            "test",
            "remote-agent",
            scope,
            Some(Content::from_text("seeded node input", ROLE_USER)),
            vec![shared_history, remote_response],
        )
        .await;
        let msg = new_message(&invocation, &A2AConfig::default())
            .await
            .unwrap();
        assert_eq!(msg.context_id, None, "want empty contextID");
        assert_eq!(texts(&msg), ["seeded node input"]);
    }

    #[tokio::test]
    async fn test_remote_agent_scoped_message_same_scope_continuity() {
        let (scope, remote) = ("workflow/node@1", "remote-agent");
        let mut first = event_from_parts("user", vec![Part::from_text("first request")]);
        first.isolation_scope = scope.into();
        let mut reply = event_from_parts(remote, vec![Part::from_text("first answer")]);
        reply.response.custom_metadata = to_custom_metadata(&a2a::new_task_id(), "ctx-same-scope");
        reply.isolation_scope = scope.into();
        let mut follow =
            event_from_parts("user", vec![Part::from_text("follow-up in the same scope")]);
        follow.isolation_scope = scope.into();

        let ictx = scoped_context(
            remote,
            scope,
            Some(Content::from_text("first request", ROLE_USER)),
            vec![first, reply, follow],
        )
        .await;
        let msg = new_message(&ictx, &A2AConfig::default()).await.unwrap();
        assert_eq!(msg.context_id.as_deref(), Some("ctx-same-scope"));
        assert_eq!(texts(&msg), ["follow-up in the same scope"]);
    }

    #[tokio::test]
    async fn test_remote_agent_scoped_message_input_required_resume() {
        let (scope, remote) = ("workflow/node@1", "remote-agent");
        let task_id = a2a::new_task_id();
        let mut call = event_from_parts(remote, vec![function_call("fc-1", "ask_user")]);
        call.response.custom_metadata = to_custom_metadata(&task_id, "ctx-resume");
        call.isolation_scope = scope.into();
        let mut answer = event_from_parts(
            "user",
            vec![function_response(
                "fc-1",
                "ask_user",
                json!({"answer": "blue"}),
            )],
        );
        answer.isolation_scope = scope.into();

        let ictx = scoped_context(
            remote,
            scope,
            Some(Content::from_text("original node input", ROLE_USER)),
            vec![call, answer],
        )
        .await;
        let msg = new_message(&ictx, &A2AConfig::default()).await.unwrap();
        assert_eq!(msg.task_id.as_deref(), Some(task_id.as_str()));
        assert_eq!(msg.context_id.as_deref(), Some("ctx-resume"));
    }

    #[tokio::test]
    async fn test_remote_agent_scoped_message_no_user_content_uses_scoped_history() {
        let (scope, remote) = ("workflow/node@1", "remote-agent");
        let mut turn = event_from_parts("user", vec![Part::from_text("in-scope question")]);
        turn.isolation_scope = scope.into();
        let ictx = scoped_context(remote, scope, None, vec![turn]).await;
        let msg = new_message(&ictx, &A2AConfig::default()).await.unwrap();
        assert!(
            !msg.parts.is_empty(),
            "scoped invocation with nil UserContent produced an empty message"
        );
    }

    #[tokio::test]
    async fn test_remote_agent_scoped_message_foreign_scope_call_id_does_not_license_in_scope_response()
     {
        let (scope, other, remote) = ("workflow/node@1", "workflow/other@1", "remote-agent");
        let mut foreign_call =
            event_from_parts(remote, vec![function_call("fc-foreign", "peer_tool")]);
        foreign_call.isolation_scope = other.into();
        let mut in_scope_response = event_from_parts(
            "user",
            vec![function_response(
                "fc-foreign",
                "peer_tool",
                json!({"ok": true}),
            )],
        );
        in_scope_response.isolation_scope = scope.into();
        let mut follow = event_from_parts("user", vec![Part::from_text("continue")]);
        follow.isolation_scope = scope.into();

        let ictx = scoped_context(
            remote,
            scope,
            None,
            vec![foreign_call, in_scope_response, follow],
        )
        .await;
        let msg = new_message(&ictx, &A2AConfig::default()).await.unwrap();
        let type_key = to_a2a_meta_key("type");
        for (i, part) in msg.parts.iter().enumerate() {
            let typed = part
                .metadata
                .as_ref()
                .and_then(|m| m.get(&type_key))
                .and_then(|v| v.as_str());
            assert_ne!(
                typed,
                Some("function_response"),
                "part[{i}] kept as function_response data"
            );
        }
        assert!(
            msg.parts.iter().any(|p| p
                .as_text()
                .is_some_and(|t| t.starts_with("Tool peer_tool returned:"))),
            "expected rewritten tool text for in-scope FR; parts={:?}",
            msg.parts
        );
    }

    #[tokio::test]
    async fn test_remote_agent_scoped_message_same_scope_positive_control() {
        let (scope, remote) = ("workflow/node@1", "remote-agent");
        let mut foreign = event_from_parts("user", vec![Part::from_text("sibling's turn")]);
        foreign.isolation_scope = "workflow/other@1".into();
        let mut in_scope = event_from_parts("user", vec![Part::from_text("my turn")]);
        in_scope.isolation_scope = scope.into();
        let ictx = scoped_context(remote, scope, None, vec![foreign, in_scope]).await;
        let msg = new_message(&ictx, &A2AConfig::default()).await.unwrap();
        assert_eq!(texts(&msg), ["my turn"]);
    }

    #[tokio::test]
    async fn test_remote_agent_scoped_message_context_id_isolation() {
        let (scope, remote) = ("workflow/node@1", "remote-agent");
        let mut foreign_reply = event_from_parts(remote, vec![Part::from_text("sibling answer")]);
        foreign_reply.response.custom_metadata =
            to_custom_metadata(&a2a::new_task_id(), "ctx-foreign-scope");
        foreign_reply.isolation_scope = "workflow/other@1".into();
        let ictx = scoped_context(
            remote,
            scope,
            Some(Content::from_text("seeded input", ROLE_USER)),
            vec![foreign_reply],
        )
        .await;
        let msg = new_message(&ictx, &A2AConfig::default()).await.unwrap();
        assert_eq!(
            msg.context_id, None,
            "foreign-scope contextID must not leak in"
        );
        assert_eq!(texts(&msg), ["seeded input"]);
    }

    #[tokio::test]
    async fn test_remote_agent_scoped_message_unscoped_does_not_replay_scoped_events() {
        let mut scoped = event_from_parts("user", vec![Part::from_text("scoped turn")]);
        scoped.isolation_scope = "workflow/node@1".into();
        let unscoped = event_from_parts("user", vec![Part::from_text("unscoped turn")]);
        let ictx = scoped_context("remote-agent", "", None, vec![scoped, unscoped]).await;
        let msg = new_message(&ictx, &A2AConfig::default()).await.unwrap();
        assert_eq!(texts(&msg), ["unscoped turn"]);
    }

    #[tokio::test]
    async fn test_remote_agent_scoped_message_re_dispatch_with_no_new_history_is_empty() {
        let (scope, remote) = ("workflow/node@1", "remote-agent");
        let mut first = event_from_parts("user", vec![Part::from_text("original input")]);
        first.isolation_scope = scope.into();
        let mut reply = event_from_parts(remote, vec![Part::from_text("answer")]);
        reply.response.custom_metadata = to_custom_metadata(&a2a::new_task_id(), "ctx-a");
        reply.isolation_scope = scope.into();
        let ictx = scoped_context(
            remote,
            scope,
            Some(Content::from_text("original input", ROLE_USER)),
            vec![first, reply],
        )
        .await;
        let msg = new_message(&ictx, &A2AConfig::default()).await.unwrap();
        assert!(
            msg.parts.is_empty(),
            "parts = {:?}, want none: a re-dispatch with nothing new in scope must not resend the original input",
            msg.parts
        );
        assert_eq!(msg.context_id.as_deref(), Some("ctx-a"));
    }

    #[tokio::test]
    async fn test_remote_agent_scoped_message_function_call_scope_isolation() {
        let (scope, remote) = ("workflow/node@1", "remote-agent");
        let foreign_task = a2a::new_task_id();
        let mut call = event_from_parts(remote, vec![function_call("fc-x", "ask_user")]);
        call.response.custom_metadata = to_custom_metadata(&foreign_task, "ctx-foreign");
        call.isolation_scope = "workflow/other@1".into();
        let mut answer = event_from_parts(
            "user",
            vec![function_response(
                "fc-x",
                "ask_user",
                json!({"answer": "blue"}),
            )],
        );
        answer.isolation_scope = "workflow/other@1".into();
        let ictx = scoped_context(
            remote,
            scope,
            Some(Content::from_text("seeded input", ROLE_USER)),
            vec![call, answer],
        )
        .await;
        let msg = new_message(&ictx, &A2AConfig::default()).await.unwrap();
        assert_ne!(msg.task_id.as_deref(), Some(foreign_task.as_str()));
        assert_ne!(msg.context_id.as_deref(), Some("ctx-foreign"));
        assert_eq!(texts(&msg), ["seeded input"]);
    }

    #[tokio::test]
    async fn test_remote_agent_part_converter() {
        let mut event = gadk_session::Event::new(&gadk_platform::Platform::default(), "");
        event.response.content = Some(Content::from_parts(
            vec![Part::from_text("KEEP"), Part::from_text("DROP")],
            gadk_genai::ROLE_MODEL,
        ));
        let converter: GenAIPartConverter = Arc::new(|_event, part| {
            let text = part.text.clone();
            Box::pin(async move {
                if text == "DROP" {
                    return Ok(None);
                }
                Ok(Some(a2a::Part::text(text)))
            })
        });
        let cfg = A2AConfig {
            genai_part_converter: Some(converter),
            ..Default::default()
        };
        let _ictx = test_invocation_context("test-agent", vec![user_hello()]).await;
        let parts = convert_parts(&cfg, &event, Some(&Default::default()))
            .await
            .unwrap();
        assert_eq!(parts.len(), 1, "expected 1 part after filtering");
        for p in &parts {
            assert_eq!(p.as_text(), Some("KEEP"));
        }
    }

    /// Go `newAgentCardServer`: a minimal card at the well-known path.
    async fn agent_card_server() -> String {
        let (base, _) = serve(|_, req| {
            if req.target == "/.well-known/agent-card.json" {
                (200, r#"{"name":"served-card"}"#.to_owned())
            } else {
                (404, String::new())
            }
        })
        .await;
        base
    }

    #[tokio::test]
    async fn test_new_agent_card_provider_accepts_uppercase_scheme() {
        let server = agent_card_server().await;
        let source = server.replacen("http://", "HTTP://", 1);
        let card = new_agent_card_provider(source.clone(), CardResolveOptions::default())()
            .await
            .unwrap_or_else(|e| panic!("NewAgentCardProvider({source:?}) error = {e}"));
        assert_eq!(card.name, "served-card");
    }

    #[tokio::test]
    async fn test_new_agent_card_provider_reads_local_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("card.json");
        std::fs::write(&path, r#"{"name":"file-card"}"#).unwrap();
        let card = new_agent_card_provider(
            path.to_string_lossy().into_owned(),
            CardResolveOptions::default(),
        )()
        .await
        .unwrap();
        assert_eq!(card.name, "file-card");
    }

    /// Serializes the tests that change the process's working directory (Go `t.Chdir`).
    static CWD: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// Restores the working directory on drop.
    struct Chdir(std::path::PathBuf);

    impl Drop for Chdir {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.0);
        }
    }

    // parity: adapted: Go's t.Chdir is process-wide in Rust too; the test holds a static mutex while the working directory is changed and restores it on drop
    #[tokio::test]
    async fn test_new_agent_card_provider_reads_relative_path_containing_colon() {
        if cfg!(windows) {
            return; // a colon cannot appear in a Windows filename
        }
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("cards:v2")).unwrap();
        let _lock = CWD.lock().await;
        let _restore = Chdir(std::env::current_dir().unwrap());
        std::env::set_current_dir(dir.path()).unwrap();

        for source in [
            "cards:v2/agent.json",
            "weird:name.json",
            "card.json:stream",
            "http:card.json",
        ] {
            std::fs::write(source, r#"{"name":"file-card"}"#).unwrap();
            let card = new_agent_card_provider(source, CardResolveOptions::default())()
                .await
                .unwrap_or_else(|e| panic!("NewAgentCardProvider({source:?}) error = {e}"));
            assert_eq!(card.name, "file-card", "{source}");
        }
    }

    // parity: adapted: Go's errors.Is(err, ErrUnsupportedCardSource) is a prefix match on the sentinel's text (no distinct error kinds)
    #[tokio::test]
    async fn test_new_agent_card_provider_rejects_non_http_scheme() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("card.json");
        std::fs::write(&path, r#"{"name":"file-card"}"#).unwrap();
        for source in [
            format!("file://{}", path.display()),
            "ftp://example.test/card.json".to_owned(),
        ] {
            let result =
                new_agent_card_provider(source.clone(), CardResolveOptions::default())().await;
            match result {
                Ok(card) => panic!("NewAgentCardProvider({source:?}) card = {card:?}, want error"),
                Err(err) => assert!(
                    err.to_string().starts_with(UNSUPPORTED_CARD_SOURCE),
                    "NewAgentCardProvider({source:?}) error = {err}"
                ),
            }
        }
    }

    // parity: adapted: Go's errors.Is(err, ErrUnsupportedCardSource) is a prefix match on the sentinel's text (no distinct error kinds)
    #[test]
    fn test_classify_card_source() {
        // (name, source, is_file, want_err)
        let tests: &[(&str, &str, bool, bool)] = &[
            ("http URL", "http://example.com", false, false),
            ("https URL", "https://example.com/cards", false, false),
            ("uppercase scheme", "HTTP://example.com", false, false),
            ("absolute path", "/etc/passwd", true, false),
            ("relative path", "cards/agent.json", true, false),
            ("windows path", r"C:\cards\agent.json", true, false),
            (
                "windows path with forward slashes",
                "C:/cards/agent.json",
                true,
                false,
            ),
            ("file scheme rejected", "file:///etc/shadow", false, true),
            (
                "other scheme rejected",
                "ftp://example.com/card.json",
                false,
                true,
            ),
            (
                "two character scheme rejected",
                "s3://bucket/card.json",
                false,
                true,
            ),
            (
                "one character scheme rejected",
                "c://cards/agent.json",
                false,
                true,
            ),
            (
                "colon in first path segment",
                "cards:v2/agent.json",
                true,
                false,
            ),
            ("colon in file name", "weird:name.json", true, false),
            ("colon suffix on file name", "card.json:stream", true, false),
            ("http scheme without slashes", "http:card.json", true, false),
            ("bare https scheme", "https:", true, false),
            ("colon before the slashes", "notes:/a://b", true, false),
            ("slash before the slashes", "dir/sub://x", true, false),
            ("tab inside scheme", "file\t://etc/hosts", true, false),
            (
                "slashes again inside an http path",
                "https://example.com/ftp://x",
                false,
                false,
            ),
            ("path with percent", "/tmp/100%.json", true, false),
            (
                "path with control character",
                "\ncards/agent.json",
                true,
                false,
            ),
            (
                "unparseable file scheme rejected",
                "file:///etc/shadow%",
                false,
                true,
            ),
            (
                "file scheme with control character rejected",
                "file:///etc/shadow\n",
                false,
                true,
            ),
            (
                "control character before scheme",
                "\nfile:///etc/shadow",
                true,
                false,
            ),
        ];
        for &(name, source, is_file, want_err) in tests {
            let got = classify_card_source(source);
            if want_err {
                match got {
                    Err(err) => assert!(
                        err.to_string().starts_with(UNSUPPORTED_CARD_SOURCE),
                        "{name}: classifyCardSource({source:?}) error = {err}"
                    ),
                    Ok(f) => panic!("{name}: classifyCardSource({source:?}) = {f}, want error"),
                }
                continue;
            }
            match got {
                Ok(f) => assert_eq!(f, is_file, "{name}: classifyCardSource({source:?})"),
                Err(err) => panic!("{name}: classifyCardSource({source:?}) error = {err}"),
            }
        }
    }

    /// A card JSON naming `ifaces` as JSON-RPC interfaces (Go encodes an `a2a.AgentCard` with
    /// `Name: "test-agent"`).
    fn card_json(ifaces: &[(String, &str)]) -> String {
        let ifaces: Vec<serde_json::Value> = ifaces
            .iter()
            .map(|(url, binding)| json!({"url": url, "protocolBinding": binding}))
            .collect();
        json!({"name": "test-agent", "supportedInterfaces": ifaces}).to_string()
    }

    /// Go `newFixedURLCardServer`: every path serves a card whose one interface is `iface_url`.
    async fn fixed_url_card_server(iface_url: &str) -> String {
        let body = card_json(&[(iface_url.to_owned(), "JSONRPC")]);
        serve(move |_, _| (200, body.clone())).await.0
    }

    /// Go `newCardServer`: every path serves a card whose interfaces come from the server's
    /// own base URL.
    async fn card_server(
        ifaces: impl Fn(&str) -> Vec<(String, &'static str)> + Send + Sync + 'static,
    ) -> String {
        serve(move |base, _| (200, card_json(&ifaces(base))))
            .await
            .0
    }

    // parity: adapted: Go's errors.Is(err, ErrUntrustedCardInterface) is a prefix match on the sentinel's text (no distinct error kinds)
    #[tokio::test]
    async fn test_new_agent_card_provider_rejects_off_origin_interface() {
        let server = fixed_url_card_server("https://attacker.example.net/rpc").await;
        let err = new_agent_card_provider(server.clone(), CardResolveOptions::default())()
            .await
            .unwrap_err();
        assert!(
            err.to_string().starts_with(UNTRUSTED_CARD_INTERFACE),
            "NewAgentCardProvider({server:?}) error = {err}"
        );
    }

    #[tokio::test]
    async fn test_new_agent_card_provider_accepts_same_origin_interface() {
        let server = card_server(|base| vec![(format!("{base}/rpc"), "JSONRPC")]).await;
        let card = new_agent_card_provider(server.clone(), CardResolveOptions::default())()
            .await
            .unwrap();
        assert_eq!(card.supported_interfaces.len(), 1);
        assert_eq!(card.supported_interfaces[0].url, format!("{server}/rpc"));
    }

    // parity: adapted: Go's errors.Is(err, ErrUntrustedCardInterface) is a prefix match on the sentinel's text (no distinct error kinds)
    #[tokio::test]
    async fn test_new_agent_card_provider_rejects_off_origin_second_interface() {
        let server = card_server(|base| {
            vec![
                (format!("{base}/rpc"), "JSONRPC"),
                ("https://attacker.example.net/rpc2".to_owned(), "GRPC"),
            ]
        })
        .await;
        let err = new_agent_card_provider(server.clone(), CardResolveOptions::default())()
            .await
            .unwrap_err();
        assert!(
            err.to_string().starts_with(UNTRUSTED_CARD_INTERFACE),
            "NewAgentCardProvider({server:?}) error = {err}"
        );
    }

    // parity: adapted: Go's errors.Is(err, ErrUntrustedCardInterface) is a prefix match on the sentinel's text (no distinct error kinds)
    #[test]
    fn test_validate_card_interface_origins() {
        let tests: &[(&str, &str, &[&str], bool)] = &[
            (
                "same origin https",
                "https://good.example.com",
                &["https://good.example.com/rpc"],
                false,
            ),
            (
                "same origin loopback http",
                "http://127.0.0.1:8080",
                &["http://127.0.0.1:8080/rpc"],
                false,
            ),
            ("no interfaces", "https://good.example.com", &[], false),
            (
                "off-origin host, same scheme",
                "https://good.example.com",
                &["https://attacker.example.net/rpc"],
                true,
            ),
            (
                "off-origin scheme, same host",
                "http://127.0.0.1:8080",
                &["https://127.0.0.1:8080/rpc"],
                true,
            ),
            (
                "second interface off-origin",
                "https://good.example.com",
                &[
                    "https://good.example.com/rpc",
                    "https://attacker.example.net/rpc2",
                ],
                true,
            ),
            (
                "plain http on non-loopback",
                "http://example.com",
                &["http://example.com/rpc"],
                true,
            ),
            (
                "off-origin port",
                "https://good.example.com:8443",
                &["https://good.example.com:9443/rpc"],
                true,
            ),
            (
                "relative interface URL",
                "https://good.example.com",
                &["/rpc"],
                true,
            ),
            (
                "empty interface URL",
                "https://good.example.com",
                &[""],
                true,
            ),
            (
                "interface host uppercase, source lowercase",
                "https://good.example.com",
                &["https://Good.Example.com/rpc"],
                false,
            ),
            (
                "source host uppercase, interface lowercase",
                "https://Good.Example.com",
                &["https://good.example.com/rpc"],
                false,
            ),
            (
                "interface has explicit default https port, source omits it",
                "https://good.example.com",
                &["https://good.example.com:443/rpc"],
                false,
            ),
            (
                "source has explicit default https port, interface omits it",
                "https://good.example.com:443",
                &["https://good.example.com/rpc"],
                false,
            ),
            (
                "interface has explicit default http port on loopback, source omits it",
                "http://localhost",
                &["http://localhost:80/rpc"],
                false,
            ),
            (
                "http on dns name with 127 prefix matching source",
                "http://127.evil.com",
                &["http://127.evil.com/rpc"],
                true,
            ),
        ];
        for &(name, source, ifaces, want_err) in tests {
            let ifaces: Vec<(String, &str)> = ifaces
                .iter()
                .map(|u| ((*u).to_owned(), "JSONRPC"))
                .collect();
            let card = crate::remoteagent::card::parse_card(card_json(&ifaces).as_bytes()).unwrap();
            let result = validate_card_interface_origins(&card, source);
            assert_eq!(result.is_err(), want_err, "{name}: {result:?}");
            if let Err(err) = result {
                assert!(
                    err.to_string().starts_with(UNTRUSTED_CARD_INTERFACE),
                    "{name}: error = {err}, want it to wrap ErrUntrustedCardInterface"
                );
            }
        }
    }

    #[test]
    fn test_is_loopback_host() {
        let tests = [
            ("IPv4 loopback exact", "127.0.0.1", true),
            (
                "IPv4 loopback, other address in the range",
                "127.5.5.5",
                true,
            ),
            ("IPv6 loopback", "::1", true),
            ("localhost", "localhost", true),
            ("localhost mixed case", "LocalHost", true),
            ("subdomain of localhost, RFC 6761", "app.localhost", true),
            (
                "dns name with a 127 prefix is not loopback",
                "127.evil.com",
                false,
            ),
            (
                "dns name containing localhost as a substring is not loopback",
                "notlocalhost.example.com",
                false,
            ),
            ("public IPv4 address", "93.184.216.34", false),
            ("empty host", "", false),
        ];
        for (name, host, want) in tests {
            assert_eq!(
                is_loopback_host(host),
                want,
                "{name}: isLoopbackHost({host:?})"
            );
        }
    }
}

/// Tests that run the remote agent against an in-process A2A server.
#[cfg(feature = "server")]
mod remote_runs {
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use futures::StreamExt as _;
    use gadk_agent::{AgentConfig, StreamingMode};
    use gadk_genai::{
        Citation, CitationMetadata, Content, FunctionCall, FunctionResponse,
        GenerateContentResponseUsageMetadata, GroundingMetadata,
        GroundingMetadataSourceFlaggingURI, Language, Outcome, Part, ROLE_MODEL, ROLE_USER,
    };
    use gadk_model::LlmResponse;
    use gadk_session::{Event, EventActions};
    use parking_lot::Mutex;
    use serde_json::{Map, Value, json};

    use super::harness::*;
    use crate::Error;
    use crate::convert::{to_a2a_meta_key, to_adk_meta_key, to_custom_metadata, to_session_event};
    use crate::remoteagent::{
        A2AConfig, A2AEventConverter, A2ARemoteTaskCleanupCallback, AfterA2ARequestCallback,
        BeforeA2ARequestCallback, CardResolveOptions, new_a2a, new_a2a_client_provider,
        new_agent_card_provider,
    };

    fn obj(v: Value) -> Map<String, Value> {
        match v {
            Value::Object(m) => m,
            other => panic!("not an object: {other}"),
        }
    }

    fn text(text: &str) -> Option<Content> {
        Some(Content::from_text(text, ROLE_MODEL))
    }

    fn resp(content: Option<Content>, partial: bool, turn_complete: bool) -> LlmResponse {
        LlmResponse {
            content,
            partial,
            turn_complete,
            ..Default::default()
        }
    }

    fn agent_msg(parts: Vec<a2a::Part>) -> a2a::Message {
        a2a::Message::new(a2a::Role::Agent, parts)
    }

    fn task_with(
        id: &str,
        ctx: &str,
        state: a2a::TaskState,
        message: Option<a2a::Message>,
        artifacts: Option<Vec<a2a::Artifact>>,
    ) -> a2a::Task {
        a2a::Task {
            id: id.to_owned(),
            context_id: ctx.to_owned(),
            status: a2a::TaskStatus {
                state,
                message,
                timestamp: None,
            },
            artifacts,
            history: None,
            metadata: None,
        }
    }

    fn artifact(parts: Vec<a2a::Part>) -> a2a::Artifact {
        a2a::Artifact {
            artifact_id: a2a::new_artifact_id(),
            name: None,
            description: None,
            parts,
            metadata: None,
            extensions: None,
        }
    }

    fn status(
        task_id: &str,
        ctx: &str,
        state: a2a::TaskState,
        message: Option<a2a::Message>,
    ) -> a2a::StreamResponse {
        let mut update = status_update(task_id, ctx, state);
        update.status.message = message;
        a2a::StreamResponse::StatusUpdate(update)
    }

    /// Go's per-event check: every event carries the original response (or is an aggregate),
    /// and only the last one the request.
    fn assert_request_response_meta(events: &[Event], name: &str) {
        for (i, event) in events.iter().enumerate() {
            let meta = &event.response.custom_metadata;
            if !meta.contains_key(&to_adk_meta_key("response")) {
                assert_eq!(
                    meta.get(&to_adk_meta_key("aggregated")),
                    Some(&Value::Bool(true)),
                    "{name}: event.CustomMetadata = {meta:?}, want response or aggregated"
                );
            }
            let want_request = i == events.len() - 1;
            assert_eq!(
                meta.contains_key(&to_adk_meta_key("request")),
                want_request,
                "{name}: event[{i}].CustomMetadata = {meta:?}"
            );
        }
    }

    /// Go `newADKEventReplay`: an agent yielding `events` stamped with the invocation.
    fn adk_event_replay(name: &str, events: Vec<Event>) -> Arc<dyn gadk_agent::Agent> {
        let author = name.to_owned();
        let events = Arc::new(events);
        gadk_agent::new_agent(AgentConfig {
            name: name.to_owned(),
            run: Some(Arc::new(move |ic| {
                let mut out = Vec::new();
                for ev in events.iter() {
                    let mut ev = ev.clone();
                    ev.invocation_id = ic.invocation_id().to_owned();
                    ev.branch = ic.branch();
                    ev.author = author.clone();
                    out.push(Ok(ev));
                }
                futures::stream::iter(out).boxed()
            })),
            ..Default::default()
        })
        .unwrap()
    }

    fn adk_event(response: LlmResponse) -> Event {
        let mut event = Event::new(&gadk_platform::Platform::default(), "");
        event.response = response;
        event
    }

    fn content_parts(parts: Vec<Part>) -> Option<Content> {
        Some(Content::from_parts(parts, ROLE_MODEL))
    }

    fn fc(name: &str, id: &str, args: Value) -> Part {
        Part {
            function_call: Some(FunctionCall {
                name: name.into(),
                id: id.into(),
                args: obj(args),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn fr(name: &str, id: &str, response: Value) -> Part {
        Part {
            function_response: Some(FunctionResponse {
                name: name.into(),
                id: id.into(),
                response: obj(response),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn test_remote_agent_adk2_adk() {
        struct Case {
            name: &'static str,
            remote_events: Vec<Event>,
            want_responses: Vec<LlmResponse>,
            want_escalate: bool,
            want_transfer: &'static str,
            allow_transfer_to_agent: bool,
            no_streaming: bool,
        }
        let case = |name, remote_events, want_responses| Case {
            name,
            remote_events,
            want_responses,
            want_escalate: false,
            want_transfer: "",
            allow_transfer_to_agent: false,
            no_streaming: false,
        };
        let turn_complete = resp(None, false, true);
        let mut escalate = adk_event(resp(text("stop"), false, false));
        escalate.actions.escalate = true;
        let mut transfer = adk_event(resp(text("stop"), false, false));
        transfer.actions.transfer_to_agent = "a-2".into();
        let mut lr_call = adk_event(resp(
            content_parts(vec![fc("create_ticket", "abc-123", json!({}))]),
            false,
            false,
        ));
        lr_call.long_running_tool_ids = vec!["abc-123".into()];
        let mut lr_resp = adk_event(resp(
            content_parts(vec![fr(
                "create_ticket",
                "abc-123",
                json!({"ticket_id": "123"}),
            )]),
            false,
            false,
        ));
        lr_resp.long_running_tool_ids = vec!["abc-123".into()];
        let metadata_response = LlmResponse {
            content: text("hello"),
            citation_metadata: Some(CitationMetadata {
                citations: vec![
                    Citation {
                        title: "Title1".into(),
                        ..Default::default()
                    },
                    Citation {
                        title: "Title2".into(),
                        ..Default::default()
                    },
                ],
            }),
            usage_metadata: Some(GenerateContentResponseUsageMetadata {
                candidates_token_count: 12,
                thoughts_token_count: 42,
                ..Default::default()
            }),
            grounding_metadata: Some(GroundingMetadata {
                source_flagging_uris: vec![GroundingMetadataSourceFlaggingURI {
                    source_id: "id1".into(),
                    ..Default::default()
                }],
                ..Default::default()
            }),
            custom_metadata: obj(json!({"nested": {"key": "value"}})),
            ..Default::default()
        };

        let cases = vec![
            case(
                "text streaming",
                vec![
                    adk_event(resp(text("hello "), true, false)),
                    adk_event(resp(text("world"), true, true)),
                    adk_event(resp(text("hello world"), false, false)),
                ],
                vec![
                    resp(text("hello "), true, false),
                    resp(text("world"), true, false),
                    resp(text("hello world"), false, false),
                    turn_complete.clone(),
                ],
            ),
            Case {
                no_streaming: true,
                ..case(
                    "text streaming - no streaming mode",
                    vec![adk_event(resp(text("hello world"), false, false))],
                    vec![resp(text("hello world"), false, true)],
                )
            },
            case(
                "code execution",
                vec![
                    adk_event(resp(
                        content_parts(vec![Part::from_executable_code(
                            "print('hello')",
                            Language::Python,
                        )]),
                        false,
                        false,
                    )),
                    adk_event(resp(
                        content_parts(vec![Part::from_code_execution_result(Outcome::Ok, "hello")]),
                        false,
                        false,
                    )),
                ],
                vec![
                    resp(
                        content_parts(vec![Part::from_executable_code(
                            "print('hello')",
                            Language::Python,
                        )]),
                        false,
                        false,
                    ),
                    resp(
                        content_parts(vec![Part::from_code_execution_result(Outcome::Ok, "hello")]),
                        false,
                        false,
                    ),
                    turn_complete.clone(),
                ],
            ),
            case(
                "function calls",
                vec![
                    adk_event(resp(
                        content_parts(vec![Part::from_function_call(
                            "get_weather",
                            obj(json!({"city": "Warsaw"})),
                        )]),
                        false,
                        false,
                    )),
                    adk_event(resp(
                        content_parts(vec![Part::from_function_response(
                            "get_weather",
                            obj(json!({"temo": "1C"})),
                        )]),
                        false,
                        false,
                    )),
                ],
                vec![
                    resp(
                        content_parts(vec![Part::from_function_call(
                            "get_weather",
                            obj(json!({"city": "Warsaw"})),
                        )]),
                        false,
                        false,
                    ),
                    resp(
                        content_parts(vec![Part::from_function_response(
                            "get_weather",
                            obj(json!({"temo": "1C"})),
                        )]),
                        false,
                        false,
                    ),
                    turn_complete.clone(),
                ],
            ),
            case(
                "files",
                vec![
                    adk_event(resp(
                        content_parts(vec![Part::from_bytes(b"hello".to_vec().into(), "text")]),
                        false,
                        false,
                    )),
                    adk_event(resp(
                        content_parts(vec![Part::from_uri("http://text.com/text.txt", "text")]),
                        false,
                        false,
                    )),
                ],
                vec![
                    resp(
                        content_parts(vec![Part::from_bytes(b"hello".to_vec().into(), "text")]),
                        false,
                        false,
                    ),
                    resp(
                        content_parts(vec![Part::from_uri("http://text.com/text.txt", "text")]),
                        false,
                        false,
                    ),
                    turn_complete.clone(),
                ],
            ),
            Case {
                want_escalate: true,
                ..case(
                    "escalation",
                    vec![escalate],
                    vec![resp(text("stop"), false, false), turn_complete.clone()],
                )
            },
            Case {
                want_transfer: "a-2",
                allow_transfer_to_agent: true,
                ..case(
                    "transfer",
                    vec![transfer],
                    vec![resp(text("stop"), false, false), turn_complete.clone()],
                )
            },
            case(
                "long-running function call",
                vec![
                    adk_event(resp(text("Hello!"), true, false)),
                    adk_event(resp(text(" I'll need your approval first:"), true, false)),
                    adk_event(resp(
                        text("Hello! I'll need your approval first:"),
                        false,
                        false,
                    )),
                    lr_call,
                    lr_resp,
                    adk_event(resp(
                        text("Waiting for the approval to continue."),
                        false,
                        false,
                    )),
                ],
                vec![
                    resp(text("Hello!"), true, false),
                    resp(text(" I'll need your approval first:"), true, false),
                    // Aggregated partial responses are emitted before a long-running call
                    resp(text("Hello! I'll need your approval first:"), false, false),
                    resp(text("Waiting for the approval to continue."), false, false),
                    resp(
                        content_parts(vec![
                            fc("create_ticket", "abc-123", json!({})),
                            fr("create_ticket", "abc-123", json!({"ticket_id": "123"})),
                        ]),
                        false,
                        true,
                    ),
                ],
            ),
            case(
                "metadata",
                vec![adk_event(metadata_response.clone())],
                vec![
                    LlmResponse {
                        custom_metadata: Map::new(),
                        ..metadata_response
                    },
                    turn_complete.clone(),
                ],
            ),
        ];

        for output_mode in [
            crate::server::OutputMode::ArtifactPerRun,
            crate::server::OutputMode::ArtifactPerEvent,
        ] {
            for tc in &cases {
                let name = format!("{} {output_mode:?}", tc.name);
                let executor = crate::server::Executor::new(crate::server::ExecutorConfig {
                    output_mode,
                    runner_config: Some(gadk_engine::runner::RunnerConfig {
                        app_name: "RemoteAgentTest".into(),
                        agent: adk_event_replay("root", tc.remote_events.clone()),
                        session_service: Arc::new(gadk_session::InMemorySessionService::new()),
                        artifact_service: None,
                        memory_service: None,
                        plugin_config: Default::default(),
                        auto_create_session: false,
                        platform: Default::default(),
                        compaction: None,
                    }),
                    ..Default::default()
                });
                let server = start_a2a_server(executor).await;
                let remote = new_a2a(A2AConfig {
                    name: "a2a".into(),
                    agent_card: Some(jsonrpc_card(&server.url)),
                    allow_transfer_to_agent: tc.allow_transfer_to_agent,
                    ..Default::default()
                })
                .unwrap();
                let mode = if tc.no_streaming {
                    StreamingMode::None
                } else {
                    StreamingMode::Sse
                };
                let ic = invocation_context(
                    "TestRemoteAgent_ADK2ADK",
                    remote.clone(),
                    vec![user_hello()],
                    mode,
                )
                .await;
                let (events, err) = run_and_collect(ic, &remote).await;
                assert!(err.is_none(), "{name}: agent.Run() error = {err:?}");
                assert_eq!(
                    without_custom_metadata(to_llm_responses(&events)),
                    tc.want_responses,
                    "{name}"
                );
                assert_request_response_meta(&events, &name);
                let last = &events.last().unwrap().actions;
                assert_eq!(last.escalate, tc.want_escalate, "{name}");
                assert_eq!(last.transfer_to_agent, tc.want_transfer, "{name}");
            }
        }
    }

    #[tokio::test]
    async fn test_remote_agent_adk2_a2_a() {
        let (tid, cid) = ("task-ids-are-replaced", "ctx-ids-are-replaced");
        let artifact_id = a2a::new_artifact_id();
        let art = |parts, append| {
            a2a::StreamResponse::ArtifactUpdate(artifact_update(
                tid,
                cid,
                &artifact_id,
                parts,
                append,
                false,
            ))
        };
        let two_texts = Some(Content::from_parts(
            vec![Part::from_text("hello"), Part::from_text("world")],
            ROLE_MODEL,
        ));
        let thought = |t: &str| {
            Some(Content::from_parts(
                vec![Part {
                    text: t.into(),
                    thought: true,
                    ..Default::default()
                }],
                ROLE_MODEL,
            ))
        };
        let cases: Vec<(&str, Vec<a2a::StreamResponse>, Vec<LlmResponse>)> = vec![
            (
                "empty message",
                vec![a2a::StreamResponse::Message(agent_msg(vec![]))],
                vec![resp(None, false, true)],
            ),
            (
                "message",
                vec![a2a::StreamResponse::Message(agent_msg(vec![
                    a2a::Part::text("hello"),
                    a2a::Part::text("world"),
                ]))],
                vec![resp(two_texts.clone(), false, true)],
            ),
            (
                "empty task",
                vec![a2a::StreamResponse::Task(task_with(
                    "",
                    "",
                    a2a::TaskState::Completed,
                    None,
                    None,
                ))],
                vec![resp(None, false, true)],
            ),
            (
                "task with status message",
                vec![a2a::StreamResponse::Task(task_with(
                    "",
                    "",
                    a2a::TaskState::Completed,
                    Some(agent_msg(vec![a2a::Part::text("hello")])),
                    None,
                ))],
                vec![resp(text("hello"), false, true)],
            ),
            (
                "task with multipart artifact",
                vec![a2a::StreamResponse::Task(task_with(
                    "",
                    "",
                    a2a::TaskState::Completed,
                    None,
                    Some(vec![artifact(vec![
                        a2a::Part::text("hello"),
                        a2a::Part::text("world"),
                    ])]),
                ))],
                vec![resp(two_texts.clone(), false, true)],
            ),
            (
                "multiple tasks",
                vec![
                    a2a::StreamResponse::Task(task_with(
                        "",
                        "",
                        a2a::TaskState::Working,
                        Some(agent_msg(vec![a2a::Part::text("hello")])),
                        None,
                    )),
                    a2a::StreamResponse::Task(task_with(
                        "",
                        "",
                        a2a::TaskState::Completed,
                        None,
                        Some(vec![artifact(vec![a2a::Part::text("world")])]),
                    )),
                ],
                vec![
                    resp(text("hello"), false, false),
                    resp(text("world"), false, true),
                ],
            ),
            (
                "task with multiple artifacts",
                vec![a2a::StreamResponse::Task(task_with(
                    "",
                    "",
                    a2a::TaskState::Completed,
                    None,
                    Some(vec![
                        artifact(vec![a2a::Part::text("hello")]),
                        artifact(vec![a2a::Part::text("world")]),
                    ]),
                ))],
                vec![resp(two_texts.clone(), false, true)],
            ),
            (
                "artifact parts translation",
                vec![
                    art(vec![a2a::Part::text("hello")], false),
                    art(vec![a2a::Part::text("world")], true),
                    status(tid, cid, a2a::TaskState::Completed, None),
                ],
                vec![
                    resp(text("hello"), true, false),
                    resp(text("world"), true, false),
                    resp(text("helloworld"), false, false),
                    resp(None, false, true),
                ],
            ),
            (
                "non-final status update messages as thoughts",
                vec![
                    status(
                        tid,
                        cid,
                        a2a::TaskState::Submitted,
                        Some(agent_msg(vec![a2a::Part::text("submitted...\n")])),
                    ),
                    status(
                        tid,
                        cid,
                        a2a::TaskState::Working,
                        Some(agent_msg(vec![a2a::Part::text("working...\n")])),
                    ),
                    status(
                        tid,
                        cid,
                        a2a::TaskState::Completed,
                        Some(agent_msg(vec![a2a::Part::text("completed!")])),
                    ),
                ],
                vec![
                    resp(thought("submitted...\n"), true, false),
                    resp(thought("working...\n"), true, false),
                    resp(text("completed!"), false, true),
                ],
            ),
            (
                "empty non-final status updates ignored",
                vec![
                    status(tid, cid, a2a::TaskState::Submitted, None),
                    status(tid, cid, a2a::TaskState::Working, None),
                    status(tid, cid, a2a::TaskState::Completed, None),
                ],
                vec![resp(None, false, true)],
            ),
            (
                "partial and non-partial event aggregation",
                vec![
                    art(vec![a2a::Part::text("1")], false),
                    art(vec![a2a::Part::text("2")], true),
                    art(vec![a2a::Part::text("3")], false),
                    art(vec![a2a::Part::text("4")], true),
                    status(
                        tid,
                        cid,
                        a2a::TaskState::Completed,
                        Some(agent_msg(vec![a2a::Part::text("5")])),
                    ),
                ],
                vec![
                    resp(text("1"), true, false),
                    resp(text("2"), true, false),
                    resp(text("3"), true, false),
                    resp(text("4"), true, false),
                    resp(text("34"), false, false),
                    resp(text("5"), false, true),
                ],
            ),
        ];

        for (name, remote_events, want) in cases {
            let server = start_a2a_server(a2a_event_replay(remote_events)).await;
            let remote = new_a2a_remote_agent("a2a", &server.url);
            let ic = invocation_context(
                "TestRemoteAgent_ADK2A2A",
                remote.clone(),
                vec![user_hello()],
                StreamingMode::Sse,
            )
            .await;
            let (events, err) = run_and_collect(ic, &remote).await;
            assert!(err.is_none(), "{name}: agent.Run() error = {err:?}");
            assert_eq!(
                without_custom_metadata(to_llm_responses(&events)),
                want,
                "{name}"
            );
            assert_request_response_meta(&events, name);
        }
    }

    type EventsFn =
        Arc<dyn Fn(&a2a_server::ExecutorContext) -> Vec<a2a::StreamResponse> + Send + Sync>;

    fn before(
        f: impl Fn(&a2a::SendMessageRequest) -> Result<Option<Event>, Error> + Send + Sync + 'static,
    ) -> BeforeA2ARequestCallback {
        Arc::new(move |_ctx, req| {
            let r = f(req);
            Box::pin(async move { r })
        })
    }

    fn after(
        f: impl Fn(
            &a2a::SendMessageRequest,
            Option<&Event>,
            Option<&a2a::A2AError>,
        ) -> Result<Option<Event>, Error>
        + Send
        + Sync
        + 'static,
    ) -> AfterA2ARequestCallback {
        Arc::new(move |_ctx, req, ev, err| {
            let r = f(req, ev, err);
            Box::pin(async move { r })
        })
    }

    fn event_with(content: Option<Content>) -> Event {
        let mut event = Event::new(&gadk_platform::Platform::default(), "");
        event.actions = EventActions::default();
        event.response.content = content;
        event
    }

    // parity: adapted: Rust request callbacks get the request and the event by shared reference; Go's in-place edits (request metadata, the result's content and metadata) become a returned replacement event
    #[tokio::test]
    async fn test_remote_agent_request_callbacks() {
        struct Case {
            name: &'static str,
            session_events: Option<Vec<Event>>,
            events: Option<EventsFn>,
            before: Vec<BeforeA2ARequestCallback>,
            after: Vec<AfterA2ARequestCallback>,
            converter: Option<A2AEventConverter>,
            want_responses: Vec<LlmResponse>,
            want_err: Option<&'static str>,
        }
        let base = |name| Case {
            name,
            session_events: None,
            events: None,
            before: vec![],
            after: vec![],
            converter: None,
            want_responses: vec![],
            want_err: None,
        };
        let hello_event = || event_with(text("hello"));
        let foo_bar = || obj(json!({"foo": "bar"}));

        let cases = vec![
            Case {
                events: Some(Arc::new(|_| {
                    vec![a2a::StreamResponse::Message(agent_msg(vec![
                        a2a::Part::text("foo"),
                    ]))]
                })),
                // Go's before callback sets req.Metadata = {"counter": 1}; the after callback
                // copies it into the result. The Rust request is immutable: the after callback
                // returns the edited event with that metadata.
                after: vec![after(|_req, result, _err| {
                    let mut result = result.unwrap().clone();
                    let t = result.response.content.as_ref().unwrap().parts[0]
                        .text
                        .clone();
                    result.response.content = text(&format!("{t}bar"));
                    result.response.custom_metadata = obj(json!({"counter": 1}));
                    Ok(Some(result))
                })],
                want_responses: vec![LlmResponse {
                    content: text("foobar"),
                    custom_metadata: obj(json!({"counter": 1})),
                    turn_complete: true,
                    ..Default::default()
                }],
                ..base("request and response modification")
            },
            Case {
                events: Some(Arc::new(|rc| {
                    let art_id = a2a::new_artifact_id();
                    let mut submitted = submitted_task(rc);
                    submitted.history = Some(vec![a2a::Message::new(
                        a2a::Role::User,
                        vec![a2a::Part::text("...")],
                    )]);
                    vec![
                        a2a::StreamResponse::Task(submitted),
                        a2a::StreamResponse::ArtifactUpdate(artifact_update(
                            &rc.task_id,
                            &rc.context_id,
                            &art_id,
                            vec![a2a::Part::text("Hello")],
                            false,
                            false,
                        )),
                        a2a::StreamResponse::ArtifactUpdate(artifact_update(
                            &rc.task_id,
                            &rc.context_id,
                            &art_id,
                            vec![a2a::Part::text(", world!")],
                            true,
                            false,
                        )),
                        status(&rc.task_id, &rc.context_id, a2a::TaskState::Completed, None),
                    ]
                })),
                after: vec![after(move |_req, result, _err| {
                    let mut result = result.unwrap().clone();
                    result.response.custom_metadata = obj(json!({"foo": "bar"}));
                    Ok(Some(result))
                })],
                want_responses: vec![
                    LlmResponse {
                        partial: true,
                        content: text("Hello"),
                        custom_metadata: foo_bar(),
                        ..Default::default()
                    },
                    LlmResponse {
                        partial: true,
                        content: text(", world!"),
                        custom_metadata: foo_bar(),
                        ..Default::default()
                    },
                    LlmResponse {
                        content: text("Hello, world!"),
                        custom_metadata: foo_bar(),
                        ..Default::default()
                    },
                    LlmResponse {
                        turn_complete: true,
                        custom_metadata: foo_bar(),
                        ..Default::default()
                    },
                ],
                ..base("after invoked for every event")
            },
            Case {
                events: Some(Arc::new(|rc| {
                    vec![
                        a2a::StreamResponse::ArtifactUpdate(artifact_update(
                            &rc.task_id,
                            &rc.context_id,
                            &a2a::new_artifact_id(),
                            vec![a2a::Part::text("Hello")],
                            false,
                            false,
                        )),
                        status(&rc.task_id, &rc.context_id, a2a::TaskState::Completed, None),
                    ]
                })),
                after: vec![after(|_, _, _| Err(Error::msg("rejected")))],
                want_err: Some("rejected"),
                ..base("after error stops the run")
            },
            Case {
                before: vec![before(move |_| Ok(Some(hello_event())))],
                want_responses: vec![resp(text("hello"), false, false)],
                ..base("request overwrite with response")
            },
            Case {
                before: vec![before(|_| Err(Error::msg("failed")))],
                want_err: Some("failed"),
                ..base("request overwrite with error")
            },
            Case {
                after: vec![after(move |_, _, _| Ok(Some(event_with(text("hello")))))],
                want_responses: vec![resp(text("hello"), false, false)],
                ..base("response overwrite")
            },
            Case {
                after: vec![after(|_, _, _| Err(Error::msg("failed")))],
                want_err: Some("failed"),
                ..base("response overwrite with error")
            },
            Case {
                before: vec![
                    before(|_| Err(Error::msg("failed"))),
                    before(|_| panic!("not called")),
                ],
                want_err: Some("failed"),
                ..base("before interceptor short-circuit")
            },
            Case {
                after: vec![
                    after(|_, _, _| Err(Error::msg("failed"))),
                    after(|_, _, _| panic!("not called")),
                ],
                want_err: Some("failed"),
                ..base("after interceptor short-circuit")
            },
            Case {
                session_events: Some(vec![]),
                after: vec![after(|req, _, _| {
                    assert!(
                        req.message.parts.is_empty(),
                        "got {} parts, expected empty message",
                        req.message.parts.len()
                    );
                    Err(Error::msg("empty session"))
                })],
                want_err: Some("empty session"),
                ..base("after interceptor for empty session")
            },
            Case {
                converter: Some(Arc::new(|_, _, _| {
                    Box::pin(async { Err(Error::msg("failed")) })
                })),
                want_err: Some("failed"),
                ..base("converter error")
            },
            Case {
                converter: Some(Arc::new(|_, _, _| {
                    Box::pin(async { Ok(Some(event_with(text("hello")))) })
                })),
                want_responses: vec![resp(text("hello"), false, false)],
                ..base("converter custom response")
            },
            Case {
                before: vec![before(|_| Err(Error::msg("before error")))],
                after: vec![after(|_, _, _| Err(Error::msg("after error")))],
                want_err: Some("after error"),
                ..base("after interceptor invoked with before result")
            },
        ];

        for tc in cases {
            let events = tc.events.clone();
            let executor = MockExecutor {
                execute: Some(Arc::new(move |ctx| {
                    let out = match &events {
                        Some(f) => f(&ctx),
                        None => vec![a2a::StreamResponse::Message(agent_msg(vec![
                            a2a::Part::text("Hi!"),
                        ]))],
                    };
                    futures::stream::iter(out.into_iter().map(Ok)).boxed()
                })),
                cancel: None,
            };
            let server = start_a2a_server(executor).await;
            let remote = new_a2a(A2AConfig {
                name: "a2a".into(),
                agent_card: Some(jsonrpc_card(&server.url)),
                before_request_callbacks: tc.before,
                after_request_callbacks: tc.after,
                converter: tc.converter,
                ..Default::default()
            })
            .unwrap();
            let session_events = tc.session_events.unwrap_or_else(|| vec![user_hello()]);
            let ic = invocation_context(
                "TestRemoteAgent_RequestCallbacks",
                remote.clone(),
                session_events,
                StreamingMode::Sse,
            )
            .await;
            let (events, err) = run_and_collect(ic, &remote).await;
            match (tc.want_err, &err) {
                (None, Some(err)) => panic!("{}: agent.Run() error = {err}, want nil", tc.name),
                (Some(want), None) => panic!("{}: agent.Run() error = nil, want {want}", tc.name),
                (Some(want), Some(err)) => assert_eq!(err.to_string(), want, "{}", tc.name),
                (None, None) => {}
            }
            assert_eq!(to_llm_responses(&events), tc.want_responses, "{}", tc.name);
        }
    }

    /// Rust-only: Go hands the after-request callback the raw error, so `errors.As` finds an A2A
    /// error wrapped by a before-request callback; here the callback's `A2AError` keeps that
    /// wrapped error's code and details, with the outer text (as on the wire, `error.rs`).
    #[tokio::test]
    async fn an_after_request_callback_sees_a_wrapped_a2a_error_with_its_code() {
        let inner = a2a::A2AError::invalid_params("bad input").with_details(vec![
            a2a::errordetails::TypedDetail::error_info("INVALID_PARAMS", "a2a-protocol.org", None),
        ]);
        let seen = Arc::new(Mutex::new(Vec::new()));
        let before_err = inner.clone();
        let seen_by_after = Arc::clone(&seen);
        let server = start_a2a_server(MockExecutor {
            execute: None,
            cancel: None,
        })
        .await;
        let remote = new_a2a(A2AConfig {
            name: "a2a".into(),
            agent_card: Some(jsonrpc_card(&server.url)),
            before_request_callbacks: vec![before(move |_| {
                Err(Error::wrap("check", before_err.clone()))
            })],
            after_request_callbacks: vec![after(move |_, _, err| {
                let err = err.unwrap();
                seen_by_after
                    .lock()
                    .push((err.code, err.message.clone(), err.details.clone()));
                Ok(None)
            })],
            ..Default::default()
        })
        .unwrap();
        let ic = invocation_context(
            "AfterCallbackSeesWrappedA2AError",
            remote.clone(),
            vec![user_hello()],
            StreamingMode::Sse,
        )
        .await;
        let (events, err) = run_and_collect(ic, &remote).await;
        assert!(events.is_empty(), "events = {events:?}");
        assert_eq!(err.unwrap().to_string(), "check: bad input");
        assert_eq!(
            *seen.lock(),
            vec![(
                inner.code,
                "check: bad input".to_owned(),
                inner.details.clone()
            )]
        );
    }

    #[tokio::test]
    async fn test_remote_agent_after_callback_runs_on_aggregated_artifact() {
        let executor = MockExecutor {
            execute: Some(Arc::new(|ctx| {
                let art_id = a2a::new_artifact_id();
                let mut submitted = submitted_task(&ctx);
                submitted.history = Some(vec![a2a::Message::new(
                    a2a::Role::User,
                    vec![a2a::Part::text("...")],
                )]);
                let events = vec![
                    a2a::StreamResponse::Task(submitted),
                    a2a::StreamResponse::ArtifactUpdate(artifact_update(
                        &ctx.task_id,
                        &ctx.context_id,
                        &art_id,
                        vec![a2a::Part::text("Hello")],
                        false,
                        false,
                    )),
                    // routes through buildNonPartialAggregation
                    a2a::StreamResponse::ArtifactUpdate(artifact_update(
                        &ctx.task_id,
                        &ctx.context_id,
                        &art_id,
                        vec![a2a::Part::text(", world!")],
                        true,
                        true,
                    )),
                    status(
                        &ctx.task_id,
                        &ctx.context_id,
                        a2a::TaskState::Completed,
                        None,
                    ),
                ];
                futures::stream::iter(events.into_iter().map(Ok)).boxed()
            })),
            cancel: None,
        };
        let server = start_a2a_server(executor).await;

        let non_partial_seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let seen = Arc::clone(&non_partial_seen);
        let remote = new_a2a(A2AConfig {
            name: "a2a".into(),
            agent_card: Some(jsonrpc_card(&server.url)),
            after_request_callbacks: vec![after(move |_, result, _| {
                if let Some(result) = result
                    && !result.response.partial
                    && let Some(content) = &result.response.content
                    && let Some(first) = content.parts.first()
                {
                    seen.lock().push(first.text.clone());
                }
                Ok(None)
            })],
            ..Default::default()
        })
        .unwrap();
        let ic = invocation_context(
            "TestRemoteAgent_AfterCallbackRunsOnAggregatedArtifact",
            remote.clone(),
            vec![user_hello()],
            StreamingMode::Sse,
        )
        .await;
        let (_, err) = run_and_collect(ic, &remote).await;
        assert!(err.is_none(), "agent.Run() error = {err:?}");
        let seen = non_partial_seen.lock().clone();
        assert!(
            seen.iter().any(|t| t == "Hello, world!"),
            "after-callback was not invoked on the aggregated artifact event; non-partial events seen = {seen:?}"
        );
    }

    #[tokio::test]
    async fn test_remote_agent_request_payload() {
        let (remote_name, not_remote_name) = ("a2a", "not-a2a");
        let user = |t: &str| event_from_parts("user", vec![Part::from_text(t)]);
        let other = |t: &str| {
            let mut e = Event::new(&gadk_platform::Platform::default(), "");
            e.author = not_remote_name.into();
            e.response.content = text(t);
            e
        };
        let msg = |parts: Vec<a2a::Part>, task: Option<&str>, ctx: Option<&str>| {
            let mut m = a2a::Message::new(a2a::Role::User, parts);
            m.task_id = task.map(str::to_owned);
            m.context_id = ctx.map(str::to_owned);
            m
        };

        let mut split_reply = Event::new(&gadk_platform::Platform::default(), "");
        split_reply.author = remote_name.into();
        split_reply.response.content = text("resp2");
        split_reply.response.custom_metadata = to_custom_metadata("", "ctx-123");

        let mut fc_event = Event::new(&gadk_platform::Platform::default(), "");
        fc_event.author = remote_name.into();
        fc_event.response.content = content_parts(vec![fc("fn", "call-1", json!({}))]);
        fc_event.response.custom_metadata = to_custom_metadata("task-1", "ctx-1");
        fc_event.long_running_tool_ids = vec!["call-1".into()];
        let mut fr_event = Event::new(&gadk_platform::Platform::default(), "");
        fr_event.author = remote_name.into();
        fr_event.response.content = content_parts(vec![
            fr("fn", "call-1", json!({"status": "pending"})),
            Part::from_text("I'll need to wait for an approval first"),
        ]);
        let mut approval = Event::new(&gadk_platform::Platform::default(), "");
        approval.author = "user".into();
        approval.response.content = Some(Content::from_parts(
            vec![
                Part::from_text("lgtm:"),
                fr("fn", "call-1", json!({"status": "approved"})),
            ],
            ROLE_USER,
        ));
        let mut fr_part = a2a::Part::data(json!({
            "id": "call-1",
            "name": "fn",
            "response": {"status": "approved"},
        }));
        fr_part.metadata = Some(HashMap::from([(
            to_a2a_meta_key("type"),
            json!("function_response"),
        )]));

        let cases = vec![
            (
                "only user message",
                vec![user_hello()],
                msg(vec![a2a::Part::text("hello")], None, None),
            ),
            (
                "history included",
                vec![user_hello(), other("hi"), user("how are you?")],
                msg(
                    vec![
                        a2a::Part::text("hello"),
                        a2a::Part::text("For context:"),
                        a2a::Part::text(format!("[{not_remote_name}] said: hi")),
                        a2a::Part::text("how are you?"),
                    ],
                    None,
                    None,
                ),
            ),
            (
                "history split by remote agent response",
                vec![
                    user("msg1"),
                    other("resp1"),
                    split_reply,
                    user("msg3"),
                    other("resp3"),
                ],
                msg(
                    vec![
                        a2a::Part::text("msg3"),
                        a2a::Part::text("For context:"),
                        a2a::Part::text(format!("[{not_remote_name}] said: resp3")),
                    ],
                    None,
                    Some("ctx-123"),
                ),
            ),
            (
                "function call response",
                vec![user("start"), fc_event, fr_event, approval],
                msg(
                    vec![a2a::Part::text("lgtm:"), fr_part],
                    Some("task-1"),
                    Some("ctx-1"),
                ),
            ),
        ];

        let server = start_a2a_server(a2a_event_replay(vec![])).await;
        for (name, session_events, want_message) in cases {
            let got = Arc::new(Mutex::new(None::<a2a::SendMessageRequest>));
            let got_cb = Arc::clone(&got);
            let remote = new_a2a(A2AConfig {
                name: remote_name.into(),
                agent_card: Some(jsonrpc_card(&server.url)),
                before_request_callbacks: vec![before(move |req| {
                    *got_cb.lock() = Some(req.clone());
                    Err(Error::msg("rejected"))
                })],
                ..Default::default()
            })
            .unwrap();
            let ic = invocation_context(
                "TestRemoteAgent_RequestPayload",
                remote.clone(),
                session_events,
                StreamingMode::Sse,
            )
            .await;
            let (_, err) = run_and_collect(ic, &remote).await;
            assert_eq!(
                err.map(|e| e.to_string()).as_deref(),
                Some("rejected"),
                "{name}"
            );
            let mut got = got.lock().clone().unwrap();
            // Go ignores the message ID.
            got.message.message_id = want_message.message_id.clone();
            let want = a2a::SendMessageRequest {
                message: want_message,
                configuration: None,
                metadata: None,
                tenant: None,
            };
            assert_eq!(
                serde_json::to_value(&got).unwrap(),
                serde_json::to_value(&want).unwrap(),
                "{name}"
            );
        }
    }

    #[tokio::test]
    async fn test_remote_agent_empty_result_for_empty_session() {
        let executor = a2a_event_replay(vec![a2a::StreamResponse::Message(agent_msg(vec![
            a2a::Part::text("will not be invoked, because input is empty"),
        ]))]);
        let agent_name = "a2a agent";
        let server = start_a2a_server(executor).await;
        let remote = new_a2a_remote_agent(agent_name, &server.url);
        let ic = invocation_context(
            "TestRemoteAgent_EmptyResultForEmptySession",
            remote.clone(),
            vec![],
            StreamingMode::Sse,
        )
        .await;
        let (events, err) = run_and_collect(ic.clone(), &remote).await;
        assert!(err.is_none(), "runAndCollect() error = {err:?}");
        assert_eq!(events.len(), 1, "{events:?}");
        let mut want = Event::new(ic.platform(), ic.invocation_id());
        want.author = agent_name.into();
        want.branch = ic.branch();
        // Go ignores ID and Timestamp.
        want.id = events[0].id.clone();
        want.timestamp = events[0].timestamp;
        assert_eq!(events[0], want);
    }

    #[tokio::test]
    async fn test_remote_agent_resolves_agent_card() {
        let executor = a2a_event_replay(vec![a2a::StreamResponse::Message(agent_msg(vec![
            a2a::Part::text("Hello!"),
        ]))]);
        let handler = Arc::new(a2a_server::DefaultRequestHandler::new(
            executor,
            a2a_server::InMemoryTaskStore::new(),
        ));
        let (listener, base) = bind_loopback().await;
        let card = serde_json::to_string(&jsonrpc_card(&format!("{base}/invoke"))).unwrap();
        let router = axum::Router::new()
            .nest_service("/invoke", a2a_server::jsonrpc::jsonrpc_router(handler))
            .route(
                "/.well-known/agent-card.json",
                axum::routing::get(move || {
                    let card = card.clone();
                    async move { card }
                }),
            );
        let card_server = serve_router_at(listener, router);

        let remote = new_a2a(A2AConfig {
            name: "a2a".into(),
            agent_card_provider: Some(new_agent_card_provider(
                card_server.url.clone(),
                CardResolveOptions::default(),
            )),
            ..Default::default()
        })
        .unwrap();
        let ic = invocation_context(
            "TestRemoteAgent_ResolvesAgentCard",
            remote.clone(),
            vec![user_hello()],
            StreamingMode::Sse,
        )
        .await;
        let (events, err) = run_and_collect(ic, &remote).await;
        assert!(err.is_none(), "agent.Run() error = {err:?}");
        assert_eq!(
            without_custom_metadata(to_llm_responses(&events)),
            vec![resp(text("Hello!"), false, true)]
        );
    }

    #[tokio::test]
    async fn test_remote_agent_error_if_no_compatible_transport() {
        let executor = a2a_event_replay(vec![a2a::StreamResponse::Message(agent_msg(vec![
            a2a::Part::text("will not be invoked!"),
        ]))]);
        let server = start_a2a_server(executor).await;
        let name = "a2a";
        let mut card = jsonrpc_card(&server.url);
        card.name = name.into();
        card.capabilities = Default::default();
        let factory = a2a_client::A2AClientFactory::builder()
            .no_defaults()
            .build();
        let remote = new_a2a(A2AConfig {
            name: name.into(),
            agent_card: Some(card),
            client_provider: Some(new_a2a_client_provider(Some(Arc::new(factory)))),
            ..Default::default()
        })
        .unwrap();
        let ic = invocation_context(
            "TestRemoteAgent_ErrorIfNoCompatibleTransport",
            remote.clone(),
            vec![user_hello()],
            StreamingMode::Sse,
        )
        .await;
        let (events, err) = run_and_collect(ic, &remote).await;
        assert!(
            err.as_ref()
                .is_some_and(|e| e.to_string().contains("no compatible transports found")),
            "agent.Run() error = {err:?}, want to contain \"no compatible transports found\""
        );
        assert!(events.is_empty(), "len(events) = {}, want 0", events.len());
    }

    #[tokio::test]
    async fn test_remote_agent_error_on_server_error() {
        let executor = MockExecutor {
            execute: Some(Arc::new(|_| {
                futures::stream::once(async { Err(a2a::A2AError::internal("mockExecutor failed")) })
                    .boxed()
            })),
            cancel: None,
        };
        let server = start_a2a_server(executor).await;
        let remote = new_a2a_remote_agent("a2a agent", &server.url);
        let ic = invocation_context(
            "TestRemoteAgent_ErrorOnServerError",
            remote.clone(),
            vec![user_hello()],
            StreamingMode::Sse,
        )
        .await;
        let (events, err) = run_and_collect(ic, &remote).await;
        assert!(err.is_some(), "agent.Run() error = nil, want non-nil");
        assert!(events.is_empty(), "len(events) = {}, want 0", events.len());
    }

    #[tokio::test]
    async fn test_remote_agent_after_request_callback_gets_nil_event_on_default_converter_error() {
        struct Case {
            name: &'static str,
            events: Vec<a2a::StreamResponse>,
            executor_err: Option<&'static str>,
            part_converter: Option<crate::convert::A2APartConverter>,
        }
        let failing: crate::convert::A2APartConverter =
            Arc::new(|_event, _part| Box::pin(async { Err(Error::msg("unsupported part")) }));
        let cases = vec![
            Case {
                name: "transport error",
                events: vec![],
                executor_err: Some("mock executor error"),
                part_converter: None,
            },
            Case {
                name: "conversion error",
                events: vec![a2a::StreamResponse::Message(agent_msg(vec![
                    a2a::Part::text("hello"),
                ]))],
                executor_err: None,
                part_converter: Some(failing),
            },
        ];
        for tc in cases {
            let name = tc.name;
            let events = tc.events;
            let executor_err = tc.executor_err;
            let executor = MockExecutor {
                execute: Some(Arc::new(move |_| {
                    let mut items: Vec<Result<a2a::StreamResponse, a2a::A2AError>> =
                        events.iter().cloned().map(Ok).collect();
                    if let Some(msg) = executor_err {
                        items.push(Err(a2a::A2AError::internal(msg)));
                    }
                    futures::stream::iter(items).boxed()
                })),
                cancel: None,
            };
            let server = start_a2a_server(executor).await;

            let calls = Arc::new(Mutex::new(Vec::<(bool, bool)>::new()));
            let seen = Arc::clone(&calls);
            let remote = new_a2a(A2AConfig {
                name: "a2a".into(),
                agent_card: Some(jsonrpc_card(&server.url)),
                a2a_part_converter: tc.part_converter,
                after_request_callbacks: vec![after(move |_, resp, err| {
                    seen.lock().push((resp.is_some(), err.is_some()));
                    Ok(None)
                })],
                ..Default::default()
            })
            .unwrap();
            let ic = invocation_context(
                "TestRemoteAgent_AfterRequestCallbackGetsNilEventOnDefaultConverterError",
                remote.clone(),
                vec![user_hello()],
                StreamingMode::Sse,
            )
            .await;
            let (_, err) = run_and_collect(ic, &remote).await;
            assert!(
                err.is_some(),
                "{name}: agent.Run() error = nil, want non-nil"
            );
            let calls = calls.lock().clone();
            assert_eq!(calls.len(), 1, "{name}: callback calls = {calls:?}, want 1");
            let (got_event, got_err) = calls[0];
            assert!(got_err, "{name}: callback err = nil, want non-nil");
            assert!(!got_event, "{name}: callback event is set, want nil");
        }
    }

    #[tokio::test]
    async fn test_remote_agent_error_if_agent_card_resolution_fails() {
        let remote = new_a2a(A2AConfig {
            name: "a2a".into(),
            agent_card_provider: Some(Arc::new(|| {
                Box::pin(async { Err(Error::msg("connection refused")) })
            })),
            ..Default::default()
        })
        .unwrap();
        let ic = invocation_context(
            "TestRemoteAgent_ErrorIfAgentCardResolutionFails",
            remote.clone(),
            vec![user_hello()],
            StreamingMode::Sse,
        )
        .await;
        let (events, err) = run_and_collect(ic, &remote).await;
        assert!(
            err.as_ref().is_some_and(|e| e
                .to_string()
                .contains("agent card resolution failed: connection refused")),
            "agent.Run() error = {err:?}, want agent card resolution failure"
        );
        assert!(events.is_empty(), "len(events) = {}, want 0", events.len());
    }

    #[tokio::test]
    async fn test_remote_agent_error_if_message_creation_fails() {
        let executor = MockExecutor {
            execute: Some(Arc::new(|_| {
                panic!("remote agent was called, want the request to fail before sending")
            })),
            cancel: None,
        };
        let server = start_a2a_server(executor).await;
        let failing: crate::convert::GenAIPartConverter =
            Arc::new(|_event, _part| Box::pin(async { Err(Error::msg("unsupported part")) }));
        let remote = new_a2a(A2AConfig {
            name: "a2a".into(),
            agent_card: Some(jsonrpc_card(&server.url)),
            genai_part_converter: Some(failing),
            ..Default::default()
        })
        .unwrap();

        // A scoped dispatch seeding its input from UserContent propagates a part conversion
        // error from new_message, as the resume path does. The path that builds the message
        // from session history logs and skips the part instead.
        let ic = scoped_context(
            "a2a",
            "workflow/node@1",
            Some(Content::from_text("seeded node input", ROLE_USER)),
            vec![],
        )
        .await;
        let (events, err) = run_and_collect(ic, &remote).await;
        assert!(
            err.as_ref()
                .is_some_and(|e| e.to_string().contains("message creation failed")),
            "agent.Run() error = {err:?}, want message creation failure"
        );
        assert!(events.is_empty(), "len(events) = {}, want 0", events.len());
    }

    #[tokio::test]
    async fn test_remote_agent_error_if_default_conversion_fails() {
        let executor = a2a_event_replay(vec![a2a::StreamResponse::Message(agent_msg(vec![
            a2a::Part::text("hello"),
        ]))]);
        let server = start_a2a_server(executor).await;
        let failing: crate::convert::A2APartConverter =
            Arc::new(|_event, _part| Box::pin(async { Err(Error::msg("unsupported part")) }));
        let remote = new_a2a(A2AConfig {
            name: "a2a".into(),
            agent_card: Some(jsonrpc_card(&server.url)),
            a2a_part_converter: Some(failing),
            ..Default::default()
        })
        .unwrap();
        let ic = invocation_context(
            "TestRemoteAgent_ErrorIfDefaultConversionFails",
            remote.clone(),
            vec![user_hello()],
            StreamingMode::Sse,
        )
        .await;
        let (events, err) = run_and_collect(ic, &remote).await;
        assert!(
            err.as_ref()
                .is_some_and(|e| e.to_string().contains("failed to convert a2aEvent")),
            "agent.Run() error = {err:?}, want conversion failure"
        );
        assert!(events.is_empty(), "len(events) = {}, want 0", events.len());
    }

    /// Go `newA2AClient(t, server)`: a plain a2a-rs JSON-RPC client.
    async fn plain_client(url: &str) -> a2a_client::A2AClient<Box<dyn a2a_client::Transport>> {
        a2a_client::A2AClientFactory::builder()
            .register(Arc::new(a2a_client::jsonrpc::JsonRpcTransportFactory::new(
                None,
            )))
            .build()
            .create_from_card(&jsonrpc_card(url))
            .await
            .unwrap()
    }

    // parity: adapted: the mock executor stops when its task is cancelled (Go: when its context is), the abandoned run's cleanup runs on a spawned task, so the test waits for the callback
    #[tokio::test]
    async fn test_remote_agent_cleanup_callback() {
        struct Case {
            name: &'static str,
            after: Vec<AfterA2ARequestCallback>,
            converter: Option<A2AEventConverter>,
            break_after: usize,
            cancel_context_after: usize,
            want_cause: &'static str,
        }
        let base = |name| Case {
            name,
            after: vec![],
            converter: None,
            break_after: 0,
            cancel_context_after: 0,
            want_cause: "",
        };
        let cases = vec![
            Case {
                after: vec![after(|_, _, _| Err(Error::msg("callback error")))],
                want_cause: "callback error",
                ..base("after request callback error")
            },
            Case {
                converter: Some(Arc::new(|ctx, _req, event| {
                    let ctx = ctx.clone();
                    let event = event.ok().cloned();
                    Box::pin(async move {
                        match event {
                            Some(a2a::StreamResponse::ArtifactUpdate(_)) => {
                                Err(Error::msg("converter error"))
                            }
                            Some(event) => to_session_event(&ctx, &event).await,
                            None => Ok(None),
                        }
                    })
                })),
                want_cause: "converter error",
                ..base("part converter error")
            },
            Case {
                cancel_context_after: 1,
                want_cause: "context canceled",
                ..base("agent run context canceled")
            },
            Case {
                break_after: 1,
                ..base("yield returns false")
            },
        ];

        for tc in cases {
            let name = tc.name;
            let (cleanup_tx, mut cleanup_rx) =
                tokio::sync::mpsc::unbounded_channel::<(String, Option<String>)>();
            let cleanup: A2ARemoteTaskCleanupCallback =
                Arc::new(move |_card, client, task, cause| {
                    let tx = cleanup_tx.clone();
                    let cause = cause.map(ToString::to_string);
                    Box::pin(async move {
                        let req = a2a::CancelTaskRequest {
                            id: task.0.clone(),
                            metadata: None,
                            tenant: None,
                        };
                        if let Err(err) = client.cancel_task(&req).await {
                            panic!("client.CancelTask() error = {err}");
                        }
                        let _ = tx.send((task.0, cause));
                    })
                });

            let (task_tx, mut task_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
            let stop = Arc::new(AtomicBool::new(false));
            let stop_exec = Arc::clone(&stop);
            let executor = MockExecutor {
                cancel: Some(Arc::new(move |ctx| {
                    stop.store(true, Ordering::SeqCst);
                    let update =
                        status_update(&ctx.task_id, &ctx.context_id, a2a::TaskState::Canceled);
                    futures::stream::iter(vec![Ok(a2a::StreamResponse::StatusUpdate(update))])
                        .boxed()
                })),
                execute: Some(Arc::new(move |ctx| {
                    let _ = task_tx.send(ctx.task_id.clone());
                    let stop = Arc::clone(&stop_exec);
                    Box::pin(async_stream::stream! {
                        yield Ok(a2a::StreamResponse::Task(submitted_task(&ctx)));
                        for _ in 0..30_000 {
                            if stop.load(Ordering::SeqCst) {
                                return;
                            }
                            let data = a2a::Part::data(json!({"foo": "bar"}));
                            yield Ok(a2a::StreamResponse::ArtifactUpdate(artifact_update(
                                &ctx.task_id,
                                &ctx.context_id,
                                &a2a::new_artifact_id(),
                                vec![data],
                                false,
                                false,
                            )));
                            tokio::time::sleep(Duration::from_millis(1)).await;
                        }
                        yield Ok(status(&ctx.task_id, &ctx.context_id, a2a::TaskState::Completed, None));
                    })
                })),
            };
            let server = start_a2a_server(executor).await;
            let remote = new_a2a(A2AConfig {
                name: "a2a".into(),
                agent_card: Some(jsonrpc_card(&server.url)),
                remote_task_cleanup_callback: Some(cleanup),
                converter: tc.converter,
                after_request_callbacks: tc.after,
                ..Default::default()
            })
            .unwrap();
            let ic = invocation_context(
                "TestRemoteAgent_CleanupCallback",
                remote.clone(),
                vec![user_hello()],
                StreamingMode::Sse,
            )
            .await;

            let mut stream = remote.run(ic.clone());
            let mut count = 0;
            while let Some(item) = stream.next().await {
                if item.is_err() {
                    break;
                }
                count += 1;
                if tc.cancel_context_after > 0 && count >= tc.cancel_context_after {
                    ic.cancellation_token().cancel();
                }
                if tc.break_after > 0 && count >= tc.break_after {
                    break;
                }
            }
            drop(stream);

            let expected_task_id = task_rx.recv().await.unwrap();
            let (cleanup_task_id, cleanup_cause) =
                tokio::time::timeout(Duration::from_secs(5), cleanup_rx.recv())
                    .await
                    .unwrap_or_else(|_| panic!("{name}: RemoteTaskCleanupCallback was not called"))
                    .unwrap();
            assert_eq!(cleanup_task_id, expected_task_id, "{name}");
            if !tc.want_cause.is_empty() {
                let cause = cleanup_cause.unwrap_or_else(|| {
                    panic!("{name}: cleanupCause is nil, want {}", tc.want_cause)
                });
                assert!(
                    cause.contains(tc.want_cause),
                    "{name}: cleanupCause = {cause}, want to contain {:?}",
                    tc.want_cause
                );
            }

            let client = plain_client(&server.url).await;
            let task = client
                .get_task(&a2a::GetTaskRequest {
                    id: expected_task_id,
                    history_length: None,
                    tenant: None,
                })
                .await
                .unwrap();
            assert_eq!(task.status.state, a2a::TaskState::Canceled, "{name}");
        }
    }
}
