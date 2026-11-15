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

//! Port of Go runner/runner_test.go.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use gadk_agent::{Agent, AgentConfig, AgentError, EventStream, InvocationContext, RunConfig};
use gadk_artifact::{ArtifactService, InMemoryArtifactService, ListRequest, LoadRequest};
use gadk_genai::{Blob, Content, FunctionCall, FunctionResponse, Part, ROLE_MODEL, ROLE_USER};
use gadk_model::LlmResponse;
use gadk_platform::Platform;
use gadk_session::{
    CreateRequest, DeleteRequest, Event, GetRequest, InMemorySessionService, Session, SessionError,
    SessionService,
};
use parking_lot::Mutex;
use rstest::rstest;

use crate::llm::{LlmAgentConfig, agent as llm_agent};
use crate::runner::run_node::build_runner_node;
use crate::runner::{PluginConfig, RunOptions, Runner, RunnerConfig};
use crate::testutil::MockModel;
use async_trait::async_trait;

fn noop_run() -> gadk_agent::AgentRunFn {
    Arc::new(|_ctx: InvocationContext| -> EventStream { Box::pin(futures::stream::empty()) })
}

fn custom_agent(name: &str) -> Arc<dyn Agent> {
    gadk_agent::new_agent(AgentConfig {
        name: name.into(),
        run: Some(noop_run()),
        ..Default::default()
    })
    .unwrap()
}

fn llm(cfg: LlmAgentConfig) -> Arc<dyn Agent> {
    llm_agent::new(cfg).unwrap()
}

fn runner_cfg(agent: Arc<dyn Agent>, svc: Arc<dyn SessionService>) -> RunnerConfig {
    RunnerConfig {
        app_name: "testApp".into(),
        agent,
        session_service: svc,
        artifact_service: None,
        memory_service: None,
        plugin_config: PluginConfig::default(),
        auto_create_session: false,
        platform: Platform::default(),
        compaction: None,
    }
}

struct AgentTree {
    root: Arc<dyn Agent>,
    no_transfer: Arc<dyn Agent>,
    allows_transfer: Arc<dyn Agent>,
}

fn agent_tree() -> AgentTree {
    let no_transfer = llm(LlmAgentConfig {
        name: "no_transfer_agent".into(),
        disallow_transfer_to_parent: true,
        ..Default::default()
    });
    let allows_transfer = llm(LlmAgentConfig {
        name: "allows_transfer_agent".into(),
        ..Default::default()
    });
    let root = llm(LlmAgentConfig {
        name: "root".into(),
        sub_agents: vec![no_transfer.clone(), allows_transfer.clone()],
        ..Default::default()
    });
    AgentTree {
        root,
        no_transfer,
        allows_transfer,
    }
}

/// Go createSession: a fresh in-memory service with `events` appended.
async fn create_session(events: Vec<Event>) -> Arc<dyn Session> {
    let svc = InMemorySessionService::new();
    let sess = svc
        .create(CreateRequest {
            app_name: "test".into(),
            user_id: "userID".into(),
            session_id: Some("sessionID".into()),
            state: None,
        })
        .await
        .unwrap();
    for mut ev in events {
        svc.append_event(&*sess, &mut ev).await.unwrap();
    }
    sess
}

fn authored(author: &str) -> Event {
    Event {
        author: author.into(),
        ..Default::default()
    }
}

async fn drain_errors(mut s: EventStream) -> Vec<AgentError> {
    let mut errs = Vec::new();
    while let Some(item) = s.next().await {
        if let Err(e) = item {
            errs.push(e);
        }
    }
    errs
}

/// (name, session events, user message, wanted agent).
type FindCase<'a> = (&'a str, Vec<Event>, Option<Content>, &'a Arc<dyn Agent>);

// parity: adapted: the Runner is built with Runner::new (Go assigns rootAgent on a bare struct)
#[tokio::test]
async fn test_runner_find_agent_to_run() {
    let tree = agent_tree();
    let call_event = Event {
        author: tree.no_transfer.name().into(),
        response: LlmResponse {
            content: Some(Content::from_parts(
                vec![Part {
                    function_call: Some(FunctionCall {
                        name: "fn_name".into(),
                        id: "fn_id".into(),
                        ..Default::default()
                    }),
                    ..Default::default()
                }],
                "",
            )),
            ..Default::default()
        },
        ..Default::default()
    };
    let fr_msg = Content::from_parts(
        vec![Part {
            function_response: Some(FunctionResponse {
                name: "fn_name".into(),
                id: "fn_id".into(),
                ..Default::default()
            }),
            ..Default::default()
        }],
        ROLE_USER,
    );
    let cases: Vec<FindCase<'_>> = vec![
        (
            "last event from agent allowing transfer",
            vec![authored("allows_transfer_agent")],
            None,
            &tree.allows_transfer,
        ),
        (
            "last event from agent not allowing transfer",
            vec![authored("no_transfer_agent")],
            None,
            &tree.root,
        ),
        ("no events from agents, call root", vec![], None, &tree.root),
        (
            "last event from user with function response",
            vec![call_event, authored("root")],
            Some(fr_msg),
            &tree.no_transfer,
        ),
    ];
    let runner = Runner::new(runner_cfg(
        tree.root.clone(),
        Arc::new(InMemorySessionService::new()),
    ))
    .unwrap();
    for (name, events, msg, want) in cases {
        let sess = create_session(events).await;
        let got = runner.find_agent_to_run(&*sess, msg.as_ref()).unwrap();
        assert!(
            Arc::ptr_eq(&got, want),
            "{name}: got {}, want {}",
            got.name(),
            want.name()
        );
    }
}

#[rstest]
#[case::disallow_for_agent_with_disallow_transfer_to_parent(
    llm(LlmAgentConfig { name: "test".into(), disallow_transfer_to_parent: true, ..Default::default() }),
    false
)]
#[case::disallow_for_non_llm_agent(
    gadk_agent::new_agent(AgentConfig { name: "test".into(), ..Default::default() }).unwrap(),
    false
)]
#[case::allow_for_the_default_llm_agent(
    llm(LlmAgentConfig { name: "test".into(), ..Default::default() }),
    true
)]
fn test_is_transferrable_across_agent_tree(#[case] agent: Arc<dyn Agent>, #[case] want: bool) {
    let runner = Runner::new(runner_cfg(
        agent.clone(),
        Arc::new(InMemorySessionService::new()),
    ))
    .unwrap();
    assert_eq!(runner.is_transferable_across_agent_tree(&agent), want);
}

// parity: adapted: the artifact service is passed in RunnerConfig instead of assigned to the private field after New
#[tokio::test]
async fn test_runner_save_input_blobs_as_artifacts() {
    let svc: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    let store = Arc::new(InMemoryArtifactService::new());
    let runner = Runner::new(RunnerConfig {
        artifact_service: Some(store.clone()),
        ..runner_cfg(custom_agent("test_agent"), svc.clone())
    })
    .unwrap();
    svc.create(CreateRequest {
        app_name: "testApp".into(),
        user_id: "testUser".into(),
        session_id: Some("testSession".into()),
        state: None,
    })
    .await
    .unwrap();

    // The message is saved only when inline data is present.
    let blob_data = "this is not blob data - René Magritte".as_bytes().to_vec();
    let msg = Content {
        role: ROLE_USER.into(),
        parts: vec![
            Part::from_text("here is a file"),
            Part {
                inline_data: Some(Blob {
                    mime_type: "application/octet-stream".into(),
                    data: blob_data.clone().into(),
                    ..Default::default()
                }),
                ..Default::default()
            },
        ],
    };
    let cfg = RunConfig {
        save_input_blobs_as_artifacts: true,
        ..Default::default()
    };
    let errs = drain_errors(runner.run(
        "testUser",
        "testSession",
        Some(msg),
        cfg,
        RunOptions::default(),
    ))
    .await;
    assert!(errs.is_empty(), "run errors: {errs:?}");

    let list = store
        .list(ListRequest {
            app_name: "testApp".into(),
            user_id: "testUser".into(),
            session_id: "testSession".into(),
        })
        .await
        .unwrap();
    assert_eq!(list.file_names.len(), 1, "expected 1 artifact");
    let saved = list.file_names[0].clone();
    assert!(saved.starts_with("artifact_"), "saved file name {saved:?}");

    let loaded = store
        .load(LoadRequest {
            app_name: "testApp".into(),
            user_id: "testUser".into(),
            session_id: "testSession".into(),
            file_name: saved.clone(),
            version: 0,
        })
        .await
        .unwrap();
    let data = loaded.part.unwrap().inline_data.unwrap().data;
    assert_eq!(&data[..], &blob_data[..]);

    let stored = svc
        .get(GetRequest {
            app_name: "testApp".into(),
            user_id: "testUser".into(),
            session_id: "testSession".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    let events = stored.events();
    assert!(!events.is_empty(), "no events in session");
    let user_event = &events[0];
    assert_eq!(user_event.author, "user");
    let parts = &user_event.response.content.as_ref().unwrap().parts;
    assert_eq!(parts.len(), 2);
    assert!(parts[1].inline_data.is_none(), "InlineData was not removed");
    assert_eq!(
        parts[1].text,
        format!("Uploaded file: {saved}. It has been saved to the artifacts")
    );
}

#[tokio::test]
async fn test_runner_plugin_modifies_user_message() {
    let got_message: Arc<Mutex<Option<Content>>> = Arc::default();
    let seen = got_message.clone();
    let run: gadk_agent::AgentRunFn = Arc::new(move |ctx: InvocationContext| -> EventStream {
        // Accessors that would fail on a callback-only context.
        let _ = ctx.agent().name().to_string();
        let _ = ctx.session().id().to_string();
        *seen.lock() = ctx.user_content().cloned();
        Box::pin(futures::stream::empty())
    });
    let agent = gadk_agent::new_agent(AgentConfig {
        name: "test_agent".into(),
        run: Some(run),
        ..Default::default()
    })
    .unwrap();
    let plugin = gadk_agent::Plugin::new(gadk_agent::PluginConfig {
        name: "message_modifier".into(),
        on_user_message_callback: Some(Arc::new(|_ctx, _msg| {
            Box::pin(async {
                Ok(Some(Content {
                    role: ROLE_USER.into(),
                    parts: vec![Part::from_text("modified")],
                }))
            })
        })),
        ..Default::default()
    });
    let svc: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    let runner = Runner::new(RunnerConfig {
        plugin_config: PluginConfig {
            plugins: vec![plugin],
            close_timeout: Duration::from_secs(1),
        },
        ..runner_cfg(agent, svc.clone())
    })
    .unwrap();
    svc.create(CreateRequest {
        app_name: "testApp".into(),
        user_id: "testUser".into(),
        session_id: Some("testSession".into()),
        state: None,
    })
    .await
    .unwrap();
    let msg = Content {
        role: ROLE_USER.into(),
        parts: vec![Part::from_text("original")],
    };
    let errs = drain_errors(runner.run(
        "testUser",
        "testSession",
        Some(msg),
        RunConfig::default(),
        RunOptions::default(),
    ))
    .await;
    assert!(errs.is_empty(), "run errors: {errs:?}");
    let got = got_message
        .lock()
        .clone()
        .expect("agent did not observe a user message");
    assert_eq!(got.parts.len(), 1);
    assert_eq!(got.parts[0].text, "modified");
}

#[test]
fn test_build_runner_node_all_agent_kinds() {
    let cases: Vec<(&str, Arc<dyn Agent>)> = vec![
        (
            "llm_agent",
            llm(LlmAgentConfig {
                name: "llm_agent".into(),
                model: Some(MockModel::new(vec![])),
                ..Default::default()
            }),
        ),
        ("custom_agent", custom_agent("custom_agent")),
        (
            "agent_with_subagents",
            gadk_agent::new_agent(AgentConfig {
                name: "agent_with_subagents".into(),
                run: Some(noop_run()),
                sub_agents: vec![custom_agent("child")],
                ..Default::default()
            })
            .unwrap(),
        ),
    ];
    for (name, agent) in cases {
        let node = build_runner_node(&agent).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(node.name(), agent.name(), "{name}");
    }
}

// parity: skipped TestBuildRunnerNode_NilAgent: Go nil-interface check; build_runner_node takes &Arc<dyn Agent>, which cannot be nil

#[tokio::test]
async fn test_new_in_memory() {
    let runner = Runner::in_memory("testApp", custom_agent("test_agent")).unwrap();
    assert!(runner.has_artifact_service());
    assert!(runner.has_memory_service());
    assert!(runner.auto_create_session());

    // A run must succeed without pre-creating the session (auto-create on).
    let msg = Content::from_parts(vec![Part::from_text("hello")], "");
    let errs = drain_errors(runner.run(
        "testUser",
        "testSession",
        Some(msg),
        RunConfig::default(),
        RunOptions::default(),
    ))
    .await;
    assert!(errs.is_empty(), "run errors: {errs:?}");
    runner
        .session_service()
        .get(GetRequest {
            app_name: "testApp".into(),
            user_id: "testUser".into(),
            session_id: "testSession".into(),
            ..Default::default()
        })
        .await
        .expect("auto-created session exists");
}

#[rstest]
#[case::auto_create_true_session_missing(true, false, false)]
#[case::auto_create_false_session_missing(false, false, true)]
#[case::auto_create_false_session_exists(false, true, false)]
#[case::auto_create_true_session_exists(true, true, false)]
#[tokio::test]
async fn test_runner_auto_create_session(
    #[case] auto_create: bool,
    #[case] setup_session: bool,
    #[case] want_err: bool,
) {
    let svc: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    if setup_session {
        svc.create(CreateRequest {
            app_name: "testApp".into(),
            user_id: "testUser".into(),
            session_id: Some("testSession".into()),
            state: None,
        })
        .await
        .unwrap();
    }
    let runner = Runner::new(RunnerConfig {
        auto_create_session: auto_create,
        ..runner_cfg(custom_agent("test_agent"), svc.clone())
    })
    .unwrap();
    let errs = drain_errors(runner.run(
        "testUser",
        "testSession",
        Some(Content::from_text("hello", ROLE_USER)),
        RunConfig::default(),
        RunOptions::default(),
    ))
    .await;
    assert_eq!(!errs.is_empty(), want_err, "errors: {errs:?}");
    if !want_err {
        svc.get(GetRequest {
            app_name: "testApp".into(),
            user_id: "testUser".into(),
            session_id: "testSession".into(),
            ..Default::default()
        })
        .await
        .expect("session exists");
    }
}

// parity: skipped TestRunner_NilEventYieldedTerminatesRun: an EventStream item is Result<Event, AgentError>; an agent cannot yield a nil event

async fn create_test_session(svc: &Arc<dyn SessionService>, app: &str, user: &str, id: &str) {
    svc.create(CreateRequest {
        app_name: app.into(),
        user_id: user.into(),
        session_id: Some(id.into()),
        state: None,
    })
    .await
    .expect("sessionService.Create()");
}

async fn get_test_session(
    svc: &Arc<dyn SessionService>,
    app: &str,
    user: &str,
    id: &str,
) -> Arc<dyn Session> {
    svc.get(GetRequest {
        app_name: app.into(),
        user_id: user.into(),
        session_id: id.into(),
        ..Default::default()
    })
    .await
    .expect("sessionService.Get()")
}

/// Go `recordingModel`: records each request and replies "ok".
fn recording_model() -> Arc<MockModel> {
    MockModel::from_contents(vec![
        Content::from_text("ok", ROLE_MODEL),
        Content::from_text("ok", ROLE_MODEL),
    ])
}

/// A custom agent whose run sets `ran` (Go `func(...) { ran = true }`).
fn flagging_agent(name: &str, ran: Arc<Mutex<bool>>) -> Arc<dyn Agent> {
    let run: gadk_agent::AgentRunFn = Arc::new(move |_ctx: InvocationContext| -> EventStream {
        *ran.lock() = true;
        Box::pin(futures::stream::empty())
    });
    gadk_agent::new_agent(AgentConfig {
        name: name.into(),
        run: Some(run),
        ..Default::default()
    })
    .unwrap()
}

fn replacing_plugin(name: &str, returned: Content) -> gadk_agent::Plugin {
    gadk_agent::Plugin::new(gadk_agent::PluginConfig {
        name: name.into(),
        on_user_message_callback: Some(Arc::new(move |_ctx, _msg| {
            let out = returned.clone();
            Box::pin(async move { Ok(Some(out)) })
        })),
        ..Default::default()
    })
}

#[rstest]
#[ignore = "8a bug: the runner keeps the inbound message's role instead of making it a user turn (Go #1877 asUserContent)"]
#[case::empty_role("")]
#[ignore = "8a bug: the runner keeps the inbound message's role instead of making it a user turn (Go #1877 asUserContent)"]
#[case::model_role(ROLE_MODEL)]
#[case::user_role(ROLE_USER)]
#[tokio::test]
async fn test_runner_inbound_message_is_user_turn(#[case] role: &str) {
    let m = recording_model();
    let a = llm(LlmAgentConfig {
        name: "a".into(),
        model: Some(m.clone()),
        ..Default::default()
    });
    let svc: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    let r = Runner::new(RunnerConfig {
        app_name: "app".into(),
        ..runner_cfg(a, svc.clone())
    })
    .unwrap();
    create_test_session(&svc, "app", "u", "s").await;

    let msg = Content {
        role: role.into(),
        parts: vec![Part::from_text("hello model")],
    };
    let errs = drain_errors(r.run(
        "u",
        "s",
        Some(msg.clone()),
        RunConfig::default(),
        RunOptions::default(),
    ))
    .await;
    assert!(errs.is_empty(), "r.Run() returned an error: {errs:?}");

    // The caller's message is left alone.
    assert_eq!(msg.role, role, "caller's msg.Role");

    // The model sees the message as a user turn.
    let reqs = m.requests();
    assert_eq!(reqs.len(), 1, "model requests");
    let sent: Vec<String> = reqs[0]
        .contents
        .iter()
        .filter(|c| c.parts.iter().any(|p| p.text == "hello model"))
        .map(|c| c.role.clone())
        .collect();
    assert_eq!(sent, vec![ROLE_USER.to_string()], "roles the model saw");

    // The stored event is a user turn too.
    let got = get_test_session(&svc, "app", "u", "s").await;
    let first = &got.events()[0];
    assert_eq!(first.author, "user");
    assert_eq!(
        first.response.content.as_ref().map(|c| c.role.as_str()),
        Some(ROLE_USER),
        "stored inbound event role"
    );
}

// parity: adapted: Rust's Runner::run takes the message by value, so it can never mutate the caller's struct; the guard checks the caller's copy keeps its blob part
#[tokio::test]
async fn test_runner_save_input_blobs_does_not_mutate_caller_message() {
    let svc: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    let r = Runner::new(RunnerConfig {
        app_name: "app".into(),
        artifact_service: Some(Arc::new(InMemoryArtifactService::new())),
        ..runner_cfg(custom_agent("test_agent"), svc.clone())
    })
    .unwrap();
    create_test_session(&svc, "app", "u", "s").await;

    let blob = Part {
        inline_data: Some(Blob {
            mime_type: "application/octet-stream".into(),
            data: b"data".to_vec().into(),
            ..Default::default()
        }),
        ..Default::default()
    };
    let msg = Content {
        role: ROLE_USER.into(),
        parts: vec![Part::from_text("file"), blob.clone()],
    };
    let cfg = RunConfig {
        save_input_blobs_as_artifacts: true,
        ..Default::default()
    };
    let errs = drain_errors(r.run("u", "s", Some(msg.clone()), cfg, RunOptions::default())).await;
    assert!(errs.is_empty(), "r.Run() returned an error: {errs:?}");
    assert_eq!(msg.parts[1], blob, "caller's message was modified");
}

#[tokio::test]
#[ignore = "8a bug: the runner keeps a role-less inbound or on_user_message message's empty role in UserContent and the stored event (Go #1877 asUserContent)"]
async fn test_runner_inbound_role_normalized_for_context_and_plugin() {
    let got_user_content: Arc<Mutex<Option<Content>>> = Arc::default();
    let seen = got_user_content.clone();
    let run: gadk_agent::AgentRunFn = Arc::new(move |ctx: InvocationContext| -> EventStream {
        *seen.lock() = ctx.user_content().cloned();
        Box::pin(futures::stream::empty())
    });
    let test_agent = gadk_agent::new_agent(AgentConfig {
        name: "test_agent".into(),
        run: Some(run),
        ..Default::default()
    })
    .unwrap();
    let svc: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    let r = Runner::new(RunnerConfig {
        app_name: "app".into(),
        ..runner_cfg(test_agent.clone(), svc.clone())
    })
    .unwrap();
    create_test_session(&svc, "app", "u", "s").await;
    let msg = Content {
        role: String::new(),
        parts: vec![Part::from_text("hi")],
    };
    let errs = drain_errors(r.run(
        "u",
        "s",
        Some(msg.clone()),
        RunConfig::default(),
        RunOptions::default(),
    ))
    .await;
    assert!(errs.is_empty(), "r.Run() returned an error: {errs:?}");
    let got = got_user_content.lock().clone();
    assert_eq!(
        got.as_ref().map(|c| c.role.as_str()),
        Some(ROLE_USER),
        "ctx.UserContent() = {got:?}"
    );

    // A plugin that returns a message with no Role still yields a user turn.
    let roleless = replacing_plugin(
        "roleless",
        Content {
            role: String::new(),
            parts: vec![Part::from_text("from plugin")],
        },
    );
    let r2 = Runner::new(RunnerConfig {
        app_name: "app".into(),
        plugin_config: PluginConfig {
            plugins: vec![roleless],
            close_timeout: Duration::from_secs(1),
        },
        ..runner_cfg(test_agent, svc.clone())
    })
    .unwrap();
    create_test_session(&svc, "app", "u", "s2").await;
    *got_user_content.lock() = None;
    let errs = drain_errors(r2.run(
        "u",
        "s2",
        Some(msg),
        RunConfig::default(),
        RunOptions::default(),
    ))
    .await;
    assert!(errs.is_empty(), "r2.Run() returned an error: {errs:?}");
    let got = got_user_content.lock().clone();
    assert!(
        got.as_ref()
            .is_some_and(|c| c.role == ROLE_USER && c.parts[0].text == "from plugin"),
        "ctx.UserContent() with plugin = {got:?}, want the plugin message with Role user"
    );
    let stored = get_test_session(&svc, "app", "u", "s2").await;
    let first = stored.events()[0].response.content.clone();
    assert!(
        first
            .as_ref()
            .is_some_and(|c| c.role == ROLE_USER && c.parts[0].text == "from plugin"),
        "stored event content = {first:?}, want the plugin message with Role user"
    );
}

fn function_call_part(name: &str, args: serde_json::Value) -> Part {
    Part {
        function_call: Some(FunctionCall {
            name: name.into(),
            args: args.as_object().cloned().unwrap_or_default(),
            ..Default::default()
        }),
        ..Default::default()
    }
}

#[rstest]
#[case::legacy_path(false, vec![function_call_part("some_tool", serde_json::json!({"key": "value"}))])]
#[case::node_path(true, vec![function_call_part("some_tool", serde_json::json!({"key": "value"}))])]
#[case::node_path_call_after_text(true, vec![Part::from_text("hi"), function_call_part("some_tool", serde_json::json!({"key": "value"}))])]
#[case::legacy_path_call_after_inline_data(false, vec![png_part(), function_call_part("some_tool", serde_json::json!({"key": "value"}))])]
#[case::node_path_call_after_inline_data(true, vec![png_part(), function_call_part("some_tool", serde_json::json!({"key": "value"}))])]
#[tokio::test]
#[ignore = "8a bug: the runner stores and runs a user message carrying a function call instead of failing with \"user message cannot contain function calls\" (Go #2302)"]
async fn test_runner_rejects_user_function_call(#[case] llm_root: bool, #[case] parts: Vec<Part>) {
    let ran = Arc::new(Mutex::new(false));
    let m = recording_model();
    let a = if llm_root {
        llm(LlmAgentConfig {
            name: "test_agent".into(),
            model: Some(m.clone()),
            ..Default::default()
        })
    } else {
        flagging_agent("test_agent", ran.clone())
    };
    let svc: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    let store = Arc::new(InMemoryArtifactService::new());
    let r = Runner::new(RunnerConfig {
        app_name: "app".into(),
        artifact_service: Some(store.clone()),
        ..runner_cfg(a, svc.clone())
    })
    .unwrap();
    create_test_session(&svc, "app", "u", "s").await;

    let msg = Content {
        role: ROLE_USER.into(),
        parts,
    };
    let cfg = RunConfig {
        save_input_blobs_as_artifacts: true,
        ..Default::default()
    };
    let errs = drain_errors(r.run("u", "s", Some(msg), cfg, RunOptions::default())).await;
    assert_eq!(
        errs.last().map(ToString::to_string).as_deref(),
        Some("user message cannot contain function calls"),
        "r.Run() errors: {errs:?}"
    );
    assert!(
        !*ran.lock() && m.requests().is_empty(),
        "agent ran = {}, model requests = {}, want no agent run and no model request",
        *ran.lock(),
        m.requests().len()
    );
    let got = get_test_session(&svc, "app", "u", "s").await;
    assert_eq!(got.events().len(), 0, "session events");
    let saved = store
        .list(ListRequest {
            app_name: "app".into(),
            user_id: "u".into(),
            session_id: "s".into(),
        })
        .await
        .unwrap();
    assert!(
        saved.file_names.is_empty(),
        "saved artifacts = {:?}",
        saved.file_names
    );
}

fn png_part() -> Part {
    Part {
        inline_data: Some(Blob {
            mime_type: "image/png".into(),
            data: vec![0x89, 0x50].into(),
            ..Default::default()
        }),
        ..Default::default()
    }
}

#[rstest]
#[case::plugin_removes_the_call(vec![function_call_part("some_tool", serde_json::json!({}))], vec![Part::from_text("from plugin")], false)]
#[ignore = "8a bug: the runner accepts a user message carrying a function call (Go #2302)"]
#[case::plugin_adds_a_call(vec![Part::from_text("hi")], vec![function_call_part("some_tool", serde_json::json!({}))], true)]
#[tokio::test]
async fn test_runner_user_function_call_checked_after_plugin(
    #[case] sent: Vec<Part>,
    #[case] returned: Vec<Part>,
    #[case] want_err: bool,
) {
    let replacer = replacing_plugin(
        "replacer",
        Content {
            role: ROLE_USER.into(),
            parts: returned,
        },
    );
    let svc: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    let r = Runner::new(RunnerConfig {
        app_name: "app".into(),
        plugin_config: PluginConfig {
            plugins: vec![replacer],
            close_timeout: Duration::from_secs(1),
        },
        ..runner_cfg(custom_agent("test_agent"), svc.clone())
    })
    .unwrap();
    create_test_session(&svc, "app", "u", "s").await;
    let errs = drain_errors(r.run(
        "u",
        "s",
        Some(Content {
            role: ROLE_USER.into(),
            parts: sent,
        }),
        RunConfig::default(),
        RunOptions::default(),
    ))
    .await;
    assert_eq!(!errs.is_empty(), want_err, "r.Run() errors: {errs:?}");
}

// parity: skipped TestRunner_UserMessageWithNilPartRuns: Content.parts is a Vec<Part>, so a user message cannot carry a nil part

/// Go `getErrSessionService`: fails `get` with a chosen error and counts `create` calls.
struct GetErrSessionService {
    inner: Arc<dyn SessionService>,
    get_err: fn() -> SessionError,
    creates: Mutex<usize>,
}

#[async_trait]
impl SessionService for GetErrSessionService {
    async fn create(&self, req: CreateRequest) -> Result<Arc<dyn Session>, SessionError> {
        *self.creates.lock() += 1;
        self.inner.create(req).await
    }
    async fn get(&self, _req: GetRequest) -> Result<Arc<dyn Session>, SessionError> {
        Err((self.get_err)())
    }
    async fn list(
        &self,
        req: gadk_session::ListRequest,
    ) -> Result<Vec<Arc<dyn Session>>, SessionError> {
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

/// Go `errors.Is(err, target)` over the source chain: the [`SessionError`] in `err`'s chain,
/// rendered.
fn session_error_in(err: &AgentError) -> Option<String> {
    let mut cur: Option<&(dyn std::error::Error + 'static)> = Some(err);
    while let Some(e) = cur {
        if let Some(se) = e.downcast_ref::<SessionError>() {
            return Some(se.to_string());
        }
        if let Some(AgentError::Session(se)) = e.downcast_ref::<AgentError>() {
            return Some(se.to_string());
        }
        cur = e.source();
    }
    None
}

fn transient_err() -> SessionError {
    SessionError::Backend("connection refused".into())
}
fn plain_not_found_err() -> SessionError {
    SessionError::Backend("not found".into())
}
fn not_found_err() -> SessionError {
    SessionError::NotFound
}

#[rstest]
#[ignore = "8a bug: with auto-create on, the runner creates the session on any get error, not only SessionError::NotFound (Go #1899)"]
#[case::transient_error_is_returned_without_create(true, transient_err as fn() -> SessionError, 0, Some("connection refused"))]
#[ignore = "8a bug: with auto-create on, the runner creates the session on any get error, not only SessionError::NotFound (Go #1899)"]
#[case::unwrapped_not_found_is_returned_without_create(true, plain_not_found_err as fn() -> SessionError, 0, Some("not found"))]
#[case::wrapped_not_found_creates_once(true, not_found_err as fn() -> SessionError, 1, None)]
#[case::auto_create_false_returns_not_found(false, not_found_err as fn() -> SessionError, 0, Some("session not found"))]
#[tokio::test]
// parity: adapted: Go's wrapped `fmt.Errorf("%w: %q", session.ErrNotFound, …)` is SessionError::NotFound; a third-party "not found" and a transient failure are SessionError::Backend; errors.Is is a source-chain search for the same SessionError
async fn test_runner_auto_create_session_get_error(
    #[case] auto_create: bool,
    #[case] get_err: fn() -> SessionError,
    #[case] want_creates: usize,
    #[case] want_err: Option<&str>,
) {
    let svc = Arc::new(GetErrSessionService {
        inner: Arc::new(InMemorySessionService::new()),
        get_err,
        creates: Mutex::new(0),
    });
    let r = Runner::new(RunnerConfig {
        auto_create_session: auto_create,
        ..runner_cfg(custom_agent("test_agent"), svc.clone())
    })
    .unwrap();
    let errs = drain_errors(r.run(
        "testUser",
        "testSession",
        Some(Content::from_parts(vec![Part::from_text("hello")], "")),
        RunConfig::default(),
        RunOptions::default(),
    ))
    .await;
    let got = errs.last().and_then(session_error_in);
    assert_eq!(got.as_deref(), want_err, "Runner.Run() errors = {errs:?}");
    assert_eq!(*svc.creates.lock(), want_creates, "Create calls");
}

#[tokio::test]
async fn test_runner_auto_create_session_empty_session_id() {
    let svc: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    let r = Runner::new(RunnerConfig {
        auto_create_session: true,
        ..runner_cfg(custom_agent("test_agent"), svc.clone())
    })
    .unwrap();
    let errs = drain_errors(r.run(
        "testUser",
        "",
        Some(Content::from_parts(vec![Part::from_text("hello")], "")),
        RunConfig::default(),
        RunOptions::default(),
    ))
    .await;
    assert!(
        errs.is_empty(),
        "Runner.Run() with an empty session ID error = {errs:?}"
    );
    let sessions = svc
        .list(gadk_session::ListRequest {
            app_name: "testApp".into(),
            user_id: "testUser".into(),
        })
        .await
        .expect("List()");
    assert_eq!(sessions.len(), 1, "List() sessions");
    assert!(
        !sessions[0].id().is_empty(),
        "created session has an empty ID"
    );
}
