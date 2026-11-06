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
use gadk_genai::{Blob, Content, FunctionCall, FunctionResponse, Part, ROLE_USER};
use gadk_model::LlmResponse;
use gadk_platform::Platform;
use gadk_session::{
    CreateRequest, Event, GetRequest, InMemorySessionService, Session, SessionService,
};
use parking_lot::Mutex;
use rstest::rstest;

use crate::llm::{LlmAgentConfig, agent as llm_agent};
use crate::runner::run_node::build_runner_node;
use crate::runner::{PluginConfig, RunOptions, Runner, RunnerConfig};
use crate::testutil::MockModel;

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
