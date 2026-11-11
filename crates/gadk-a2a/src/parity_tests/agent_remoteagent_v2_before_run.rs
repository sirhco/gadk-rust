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

//! Ports of ADK-Go `agent/remoteagent/v2/before_run_test.go`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use futures::StreamExt as _;
use futures::stream::BoxStream;
use gadk_agent::{
    Agent, BeforeAgentCallback, BeforeRunCallback, OnEventCallback, Plugin, PluginConfig,
    RunConfig, StreamingMode,
};
use gadk_engine::runner::{self, RunOptions, Runner, RunnerConfig};
use gadk_genai::{Content, FunctionCall, FunctionResponse, Part, ROLE_MODEL, ROLE_USER};
use gadk_platform::Platform;
use gadk_session::{Event, InMemorySessionService};
use parking_lot::Mutex;
use serde_json::{Map, Value, json};

use crate::Error;
use crate::convert::to_session_event;
use crate::remoteagent::{
    A2AClient, A2AClientProvider, A2AConfig, A2AEventConverter, AfterA2ARequestCallback,
    BeforeA2ARequestCallback, ClientContext, new_a2a,
};

/// Go `&a2a.AgentCard{}`.
fn empty_card() -> a2a::AgentCard {
    serde_json::from_value(json!({
        "name": "", "version": "", "supportedInterfaces": [], "capabilities": {},
    }))
    .unwrap()
}

/// Go `beforeRunClient`: answers every send with an agent message carrying `context_id`.
#[derive(Default)]
struct BeforeRunClient {
    context_id: String,
    calls: AtomicUsize,
    last_message: Mutex<Option<a2a::Message>>,
}

impl BeforeRunClient {
    fn new(context_id: &str) -> Arc<Self> {
        Arc::new(Self {
            context_id: context_id.to_owned(),
            ..Default::default()
        })
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn last_message(&self) -> a2a::Message {
        self.last_message.lock().clone().unwrap()
    }

    fn reply(&self, req: &a2a::SendMessageRequest) -> a2a::Message {
        self.calls.fetch_add(1, Ordering::SeqCst);
        *self.last_message.lock() = Some(req.message.clone());
        let mut msg = a2a::Message::new(a2a::Role::Agent, vec![a2a::Part::text("remote reply")]);
        msg.context_id = (!self.context_id.is_empty()).then(|| self.context_id.clone());
        msg
    }
}

#[async_trait::async_trait]
impl A2AClient for BeforeRunClient {
    async fn send_message(
        &self,
        req: &a2a::SendMessageRequest,
    ) -> Result<a2a::SendMessageResponse, a2a::A2AError> {
        Ok(a2a::SendMessageResponse::Message(self.reply(req)))
    }

    async fn send_streaming_message(
        &self,
        req: &a2a::SendMessageRequest,
    ) -> Result<BoxStream<'static, Result<a2a::StreamResponse, a2a::A2AError>>, a2a::A2AError> {
        let msg = self.reply(req);
        Ok(futures::stream::iter(vec![Ok(a2a::StreamResponse::Message(msg))]).boxed())
    }

    async fn cancel_task(&self, _req: &a2a::CancelTaskRequest) -> Result<a2a::Task, a2a::A2AError> {
        Err(a2a::A2AError::internal("not implemented"))
    }

    async fn destroy(&self) -> Result<(), a2a::A2AError> {
        Ok(())
    }
}

/// Go `ClientProvider: func(...) { return client, nil }`.
struct FixedProvider(Arc<dyn A2AClient>);

#[async_trait::async_trait]
impl A2AClientProvider for FixedProvider {
    async fn create_client(
        &self,
        _cx: ClientContext<'_>,
        _card: &a2a::AgentCard,
    ) -> Result<Arc<dyn A2AClient>, Error> {
        Ok(Arc::clone(&self.0))
    }
}

fn new_runner(app_name: &str, agent: Arc<dyn Agent>, plugins: Vec<Plugin>) -> Runner {
    Runner::new(RunnerConfig {
        app_name: app_name.to_owned(),
        agent,
        session_service: Arc::new(InMemorySessionService::new()),
        artifact_service: None,
        memory_service: None,
        plugin_config: runner::PluginConfig {
            plugins,
            close_timeout: Duration::from_secs(1),
        },
        auto_create_session: true,
        platform: Platform::default(),
        compaction: None,
    })
    .unwrap()
}

/// Runs one turn, failing on an error item or an error event; returns the events.
async fn run_turn(r: &Runner, content: Content, mode: StreamingMode) -> Vec<Event> {
    let mut stream = r.run(
        "u",
        "s",
        Some(content),
        RunConfig {
            streaming_mode: mode,
            ..Default::default()
        },
        RunOptions::default(),
    );
    let mut events = Vec::new();
    while let Some(item) = stream.next().await {
        let ev = item.unwrap();
        assert!(
            ev.response.error_code.is_empty() && ev.response.error_message.is_empty(),
            "run emitted an error event: {ev:?}"
        );
        events.push(ev);
    }
    events
}

fn texts(msg: &a2a::Message) -> Vec<String> {
    msg.parts
        .iter()
        .map(|p| p.as_text().unwrap_or_default().to_owned())
        .collect()
}

fn obj(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        other => panic!("not an object: {other}"),
    }
}

const MODES: [StreamingMode; 2] = [StreamingMode::None, StreamingMode::Sse];

#[tokio::test]
async fn test_before_run_reply_preserves_remote_session() {
    let convert_response: A2AEventConverter = Arc::new(|ctx, _req, ev| {
        let ctx = ctx.clone();
        let ev = ev.cloned().map_err(Clone::clone);
        Box::pin(async move {
            match ev {
                Ok(ev) => to_session_event(&ctx, &ev).await,
                Err(err) => Err(Error::A2A(err)),
            }
        })
    });
    struct Case {
        name: &'static str,
        context_id: &'static str,
        converter: Option<A2AEventConverter>,
        replace: bool,
        function_call: bool,
    }
    let cases = [
        Case {
            name: "stateful",
            context_id: "remote-context",
            converter: None,
            replace: false,
            function_call: false,
        },
        Case {
            name: "stateless",
            context_id: "",
            converter: None,
            replace: false,
            function_call: false,
        },
        Case {
            name: "converter without metadata",
            context_id: "",
            converter: Some(convert_response),
            replace: false,
            function_call: false,
        },
        Case {
            name: "after callback without metadata",
            context_id: "",
            converter: None,
            replace: true,
            function_call: false,
        },
        Case {
            name: "cached function call",
            context_id: "remote-context",
            converter: None,
            replace: false,
            function_call: true,
        },
    ];
    for tc in &cases {
        for mode in MODES {
            let name = format!("{}/{mode:?}", tc.name);
            let client = BeforeRunClient::new(tc.context_id);
            let mut cfg = A2AConfig {
                name: "remote".into(),
                agent_card: Some(empty_card()),
                converter: tc.converter.clone(),
                client_provider: Some(Arc::new(FixedProvider(client.clone()))),
                ..Default::default()
            };
            if tc.replace {
                let cb: AfterA2ARequestCallback = Arc::new(|_ctx, _req, ev, err| {
                    let out = ev.map(|ev| {
                        let mut out = ev.clone();
                        out.response.custom_metadata = Map::new();
                        out
                    });
                    let err = err.cloned();
                    Box::pin(async move {
                        match err {
                            Some(err) => Err(Error::A2A(err)),
                            None => Ok(out),
                        }
                    })
                });
                cfg.after_request_callbacks = vec![cb];
            }
            let remote = new_a2a(cfg).unwrap();

            let turn = Arc::new(AtomicUsize::new(0));
            let on_event_turn = Arc::clone(&turn);
            let on_event: OnEventCallback = Arc::new(move |_ctx, ev| {
                let turn = on_event_turn.load(Ordering::SeqCst);
                Box::pin(async move {
                    if turn != 2 {
                        return Ok(None);
                    }
                    let mut out = ev;
                    out.response.custom_metadata = obj(json!({"cache": true}));
                    Ok(Some(out))
                })
            });
            let before_turn = Arc::clone(&turn);
            let function_call = tc.function_call;
            let before_run: BeforeRunCallback = Arc::new(move |_ctx| {
                let turn = before_turn.fetch_add(1, Ordering::SeqCst) + 1;
                Box::pin(async move {
                    if turn == 2 {
                        if function_call {
                            return Ok(Some(Content::from_parts(
                                vec![Part {
                                    function_call: Some(FunctionCall {
                                        id: "cached-call".into(),
                                        name: "lookup".into(),
                                        ..Default::default()
                                    }),
                                    ..Default::default()
                                }],
                                ROLE_MODEL,
                            )));
                        }
                        return Ok(Some(Content::from_text("cached reply", ROLE_MODEL)));
                    }
                    Ok(None)
                })
            });
            let cache = Plugin::new(PluginConfig {
                name: "cache".into(),
                on_event_callback: Some(on_event),
                before_run_callback: Some(before_run),
                ..Default::default()
            });
            let r = new_runner("test", remote, vec![cache]);
            for (i, question) in ["first question", "cached question", "next question"]
                .into_iter()
                .enumerate()
            {
                let mut content = Content::from_text(question, ROLE_USER);
                if tc.function_call && i == 2 {
                    content = Content::from_parts(
                        vec![Part {
                            function_response: Some(FunctionResponse {
                                id: "cached-call".into(),
                                name: "lookup".into(),
                                response: obj(json!({"ok": true})),
                                ..Default::default()
                            }),
                            ..Default::default()
                        }],
                        ROLE_USER,
                    );
                }
                run_turn(&r, content, mode).await;
            }
            assert_eq!(client.calls(), 2, "{name}: remote calls");
            let msg = client.last_message();
            assert_eq!(
                msg.context_id.as_deref().unwrap_or_default(),
                tc.context_id,
                "{name}: remote context was not preserved"
            );
            assert!(
                msg.task_id.is_none(),
                "{name}: local reply incorrectly resumed a remote task"
            );
            let want: Vec<&str> = if tc.function_call {
                vec![
                    "cached question",
                    "For context:",
                    "[remote] called tool lookup with parameters: map[]",
                    r#"Tool lookup returned: {"ok":true}"#,
                ]
            } else {
                vec![
                    "cached question",
                    "For context:",
                    "[remote] said: cached reply",
                    "next question",
                ]
            };
            assert_eq!(texts(&msg), want, "{name}: forwarded parts");
        }
    }
}

/// Go `beforeRunPeerClient`: an a2a-rs request handler used directly as the client.
#[cfg(feature = "server")]
struct PeerClient(a2a_server::DefaultRequestHandler);

#[cfg(feature = "server")]
#[async_trait::async_trait]
impl A2AClient for PeerClient {
    async fn send_message(
        &self,
        req: &a2a::SendMessageRequest,
    ) -> Result<a2a::SendMessageResponse, a2a::A2AError> {
        use a2a_server::RequestHandler as _;
        self.0.send_message(&Default::default(), req.clone()).await
    }

    async fn send_streaming_message(
        &self,
        req: &a2a::SendMessageRequest,
    ) -> Result<BoxStream<'static, Result<a2a::StreamResponse, a2a::A2AError>>, a2a::A2AError> {
        use a2a_server::RequestHandler as _;
        self.0
            .send_streaming_message(&Default::default(), req.clone())
            .await
    }

    async fn cancel_task(&self, req: &a2a::CancelTaskRequest) -> Result<a2a::Task, a2a::A2AError> {
        use a2a_server::RequestHandler as _;
        self.0.cancel_task(&Default::default(), req.clone()).await
    }

    async fn destroy(&self) -> Result<(), a2a::A2AError> {
        Ok(())
    }
}

// parity: adapted: Go's OnEventCallback mutates the event in place and returns nil; a gadk OnEventCallback owns its event, so it returns the modified copy
#[cfg(feature = "server")]
#[tokio::test]
#[ignore = "needs plan 5b Task 7's server::Executor; Task 9 un-ignores"]
async fn test_peer_before_run_reply_preserves_remote_session() {
    use super::agent_remoteagent_v2_a2a_agent::harness::MockExecutor;
    use crate::convert::{event_to_message, get_a2a_task_info, to_genai_parts};
    use crate::server::{Executor, ExecutorConfig};

    for mode in MODES {
        let peer = gadk_agent::new_agent(gadk_agent::AgentConfig {
            name: "peer".into(),
            ..Default::default()
        })
        .unwrap();
        let before_run: BeforeRunCallback = Arc::new(|_ctx| {
            Box::pin(async { Ok(Some(Content::from_text("remote cached reply", ROLE_MODEL))) })
        });
        let on_event: OnEventCallback = Arc::new(|_ctx, ev| {
            Box::pin(async move {
                let mut out = ev;
                out.response.custom_metadata = obj(json!({"cache_hit": true}));
                Ok(Some(out))
            })
        });
        let cache = Plugin::new(PluginConfig {
            name: "peer cache".into(),
            before_run_callback: Some(before_run),
            on_event_callback: Some(on_event),
            ..Default::default()
        });
        let peer_config = RunnerConfig {
            app_name: "peer".into(),
            agent: peer,
            session_service: Arc::new(InMemorySessionService::new()),
            artifact_service: None,
            memory_service: None,
            plugin_config: runner::PluginConfig {
                plugins: vec![cache],
                close_timeout: Duration::from_secs(1),
            },
            auto_create_session: true,
            platform: Platform::default(),
            compaction: None,
        };
        let handler = if mode == StreamingMode::None {
            let peer_runner = Arc::new(Runner::new(peer_config).unwrap());
            let executor = MockExecutor {
                execute: Some(Arc::new(move |req: a2a_server::ExecutorContext| {
                    let peer_runner = Arc::clone(&peer_runner);
                    Box::pin(async_stream::stream! {
                        let message = req.message.clone().unwrap();
                        let parts = match to_genai_parts(&message.parts) {
                            Ok(parts) => parts,
                            Err(err) => {
                                yield Err(a2a::A2AError::from(err));
                                return;
                            }
                        };
                        let mut events = peer_runner.run(
                            "u",
                            &req.context_id,
                            Some(Content::from_parts(parts, ROLE_USER)),
                            RunConfig::default(),
                            RunOptions::default(),
                        );
                        while let Some(item) = events.next().await {
                            let ev = match item {
                                Ok(ev) => ev,
                                Err(err) => {
                                    yield Err(a2a::A2AError::internal(err.to_string()));
                                    return;
                                }
                            };
                            let mut msg = match event_to_message(&ev) {
                                Ok(Some(msg)) => msg,
                                Ok(None) => continue,
                                Err(err) => {
                                    yield Err(a2a::A2AError::from(err));
                                    return;
                                }
                            };
                            msg.context_id = Some(req.context_id.clone());
                            yield Ok(a2a::StreamResponse::Message(msg));
                        }
                    })
                })),
                cancel: None,
            };
            a2a_server::DefaultRequestHandler::new(executor, a2a_server::InMemoryTaskStore::new())
        } else {
            let executor = Executor::new(ExecutorConfig {
                runner_config: Some(peer_config),
                ..Default::default()
            });
            a2a_server::DefaultRequestHandler::new(executor, a2a_server::InMemoryTaskStore::new())
        };
        let client = Arc::new(PeerClient(handler));
        let last_message: Arc<Mutex<Option<a2a::Message>>> = Arc::new(Mutex::new(None));
        let recorder = Arc::clone(&last_message);
        let record: BeforeA2ARequestCallback = Arc::new(move |_ctx, req| {
            *recorder.lock() = Some(req.message.clone());
            Box::pin(async { Ok(None) })
        });
        let mut cfg = A2AConfig {
            name: "remote".into(),
            agent_card: Some(empty_card()),
            client_provider: Some(Arc::new(FixedProvider(client))),
            before_request_callbacks: vec![record],
            ..Default::default()
        };
        if mode == StreamingMode::Sse {
            // A content-only converter must not rely on a trailing status event to preserve
            // context.
            let converter: A2AEventConverter = Arc::new(|ctx, _req, ev| {
                let ctx = ctx.clone();
                let ev = ev.cloned().map_err(Clone::clone);
                Box::pin(async move {
                    let ev = ev.map_err(Error::A2A)?;
                    let converted = to_session_event(&ctx, &ev).await?;
                    Ok(converted.filter(|c| c.response.content.is_some()))
                })
            });
            cfg.converter = Some(converter);
        }
        let remote = new_a2a(cfg).unwrap();
        let r = new_runner("client", remote, vec![]);
        let mut remote_context = String::new();
        for (i, question) in ["first question", "second question"]
            .into_iter()
            .enumerate()
        {
            for ev in run_turn(&r, Content::from_text(question, ROLE_USER), mode).await {
                if ev.response.content.is_some() && i == 0 {
                    remote_context = get_a2a_task_info(&ev).1;
                    assert_eq!(
                        ev.response.custom_metadata.get("cache_hit"),
                        Some(&Value::Bool(true)),
                        "{mode:?}: peer custom metadata was not preserved"
                    );
                }
            }
        }
        let last = last_message.lock().clone().unwrap();
        assert!(
            !remote_context.is_empty()
                && last.context_id.as_deref() == Some(remote_context.as_str()),
            "{mode:?}: peer BeforeRun reply did not preserve the remote context"
        );
        assert_eq!(
            texts(&last),
            vec!["second question"],
            "{mode:?}: peer BeforeRun reply was replayed as a local reply"
        );
    }
}

#[tokio::test]
async fn test_callback_reply_keeps_history_boundary() {
    for kind in ["before agent", "plugin before agent", "before request"] {
        for mode in MODES {
            let name = format!("{kind}/{mode:?}");
            let client = BeforeRunClient::new("remote-context");
            let mut cfg = A2AConfig {
                name: "remote".into(),
                agent_card: Some(empty_card()),
                client_provider: Some(Arc::new(FixedProvider(client.clone()))),
                ..Default::default()
            };
            let denied = Arc::new(AtomicUsize::new(0));
            let refusal = Content::from_text("Request withheld.", ROLE_MODEL);
            let guard_denied = Arc::clone(&denied);
            let guard_refusal = refusal.clone();
            let guard: BeforeAgentCallback = Arc::new(move |ctx| {
                let blocked = ctx
                    .user_content()
                    .is_some_and(|c| c.parts.iter().any(|p| p.text == "blocked input"));
                if blocked {
                    guard_denied.fetch_add(1, Ordering::SeqCst);
                }
                let refusal = guard_refusal.clone();
                Box::pin(async move { Ok(blocked.then_some(refusal)) })
            });
            let mut plugins = Vec::new();
            match kind {
                "before agent" => cfg.before_agent_callbacks = vec![guard],
                "plugin before agent" => plugins.push(Plugin::new(PluginConfig {
                    name: "guard".into(),
                    before_agent_callback: Some(guard),
                    ..Default::default()
                })),
                _ => {
                    let req_denied = Arc::clone(&denied);
                    let cb: BeforeA2ARequestCallback = Arc::new(move |ctx, req| {
                        let blocked = req
                            .message
                            .parts
                            .iter()
                            .any(|p| p.as_text() == Some("blocked input"));
                        let out = blocked.then(|| {
                            req_denied.fetch_add(1, Ordering::SeqCst);
                            let mut ev = Event::new(ctx.platform(), "refusal");
                            ev.author = "remote".into();
                            ev.response.content = Some(refusal.clone());
                            ev
                        });
                        Box::pin(async move { Ok(out) })
                    });
                    cfg.before_request_callbacks = vec![cb];
                }
            }
            let remote = new_a2a(cfg).unwrap();
            let r = new_runner("test", remote, plugins);
            let mut want_calls = 0;
            for (i, question) in [
                "first question",
                "blocked input",
                "next question",
                "last question",
            ]
            .into_iter()
            .enumerate()
            {
                run_turn(&r, Content::from_text(question, ROLE_USER), mode).await;
                if i != 1 {
                    want_calls += 1;
                }
                assert_eq!(client.calls(), want_calls, "{name}: turn {i}: remote calls");
                if i == 1 {
                    continue;
                }
                let msg = client.last_message();
                assert_eq!(
                    texts(&msg),
                    vec![question],
                    "{name}: turn {i}: callback boundary replayed earlier content"
                );
                let want_context = if i == 3 { "remote-context" } else { "" };
                assert_eq!(
                    msg.context_id.as_deref().unwrap_or_default(),
                    want_context,
                    "{name}: turn {i}: context ID did not follow the latest history boundary"
                );
            }
            assert_eq!(denied.load(Ordering::SeqCst), 1, "{name}: denied turns");
        }
    }
}
