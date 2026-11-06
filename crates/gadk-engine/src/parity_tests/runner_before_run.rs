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

//! Port of Go runner/before_run_test.go.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use gadk_agent::{
    Agent, AgentConfig, AgentError, EventStream, InvocationContext, Plugin, RunConfig,
};
use gadk_genai::{Content, ROLE_MODEL, ROLE_USER};
use gadk_platform::Platform;
use gadk_session::{
    CreateRequest, DeleteRequest, Event, GetRequest, InMemorySessionService, ListRequest, Session,
    SessionError, SessionService,
};
use parking_lot::Mutex;
use serde_json::{Map, Value, json};

use crate::llm::{LlmAgentConfig, agent as llm_agent};
use crate::runner::{BEFORE_RUN_REPLY_KEY, PluginConfig, RunOptions, Runner, RunnerConfig};
use crate::testutil::MockModel;

/// Go beforeRunSessionService: counts non-user appends and optionally fails them.
struct BeforeRunSessionService {
    inner: InMemorySessionService,
    fail: Option<&'static str>,
    reply_appends: AtomicUsize,
}

#[async_trait]
impl SessionService for BeforeRunSessionService {
    async fn create(&self, req: CreateRequest) -> Result<Arc<dyn Session>, SessionError> {
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
        if event.author != "user" {
            self.reply_appends.fetch_add(1, Ordering::SeqCst);
            if let Some(msg) = self.fail {
                return Err(SessionError::Backend(msg.into()));
            }
        }
        self.inner.append_event(session, event).await
    }
}

#[derive(Clone, Copy, Default)]
struct Case {
    name: &'static str,
    before_err: bool,
    on_event_err: bool,
    append_err: bool,
    with_content: bool,
    replace: bool,
    partial: bool,
    rewrite_user: bool,
    without_user: bool,
}

fn cases() -> Vec<Case> {
    let c = Case::default();
    vec![
        Case {
            name: "reply",
            with_content: true,
            ..c
        },
        Case {
            name: "on event replaces reply",
            with_content: true,
            replace: true,
            ..c
        },
        Case {
            name: "partial reply is not persisted",
            with_content: true,
            partial: true,
            ..c
        },
        Case {
            name: "before run error",
            before_err: true,
            ..c
        },
        Case {
            name: "before run error takes precedence over reply",
            before_err: true,
            with_content: true,
            ..c
        },
        Case {
            name: "on event error",
            with_content: true,
            on_event_err: true,
            ..c
        },
        Case {
            name: "append error",
            with_content: true,
            append_err: true,
            ..c
        },
        Case {
            name: "rewritten user input",
            with_content: true,
            rewrite_user: true,
            ..c
        },
        Case {
            name: "no user message",
            with_content: true,
            without_user: true,
            ..c
        },
    ]
}

#[tokio::test]
async fn test_runner_before_run_short_circuit() {
    for kind in ["custom", "llm"] {
        for tc in cases() {
            run_case(kind, tc).await;
        }
    }
}

fn obj(v: Value) -> Map<String, Value> {
    let Value::Object(m) = v else { unreachable!() };
    m
}

async fn run_case(kind: &str, tc: Case) {
    let label = format!("{kind}/{}", tc.name);
    let agent: Arc<dyn Agent> = if kind == "llm" {
        llm_agent::new(LlmAgentConfig {
            name: "assistant".into(),
            model: Some(MockModel::new(vec![])),
            ..Default::default()
        })
        .unwrap()
    } else {
        gadk_agent::new_agent(AgentConfig {
            name: "assistant".into(),
            run: Some(Arc::new(|_ctx: InvocationContext| -> EventStream {
                Box::pin(futures::stream::empty())
            })),
            ..Default::default()
        })
        .unwrap()
    };

    let mut user_content = Some(Content::from_text("original question", ROLE_USER));
    let want_user_content = if tc.rewrite_user {
        Content::from_text("sanitized question", ROLE_USER)
    } else {
        Content::from_text("original question", ROLE_USER)
    };
    let mut want_user_events = 1;
    if tc.without_user {
        user_content = None;
        want_user_events = 0;
    }
    let cached_reply = Content::from_text("cached answer", ROLE_MODEL);
    let want_reply = if tc.replace {
        Content::from_text("modified answer", ROLE_MODEL)
    } else {
        cached_reply.clone()
    };

    let on_event_calls = Arc::new(AtomicUsize::new(0));
    let after_run_calls = Arc::new(AtomicUsize::new(0));
    let ran_agent = Arc::new(AtomicBool::new(false));
    let plugin_reply: Arc<Mutex<Option<Event>>> = Arc::default();
    let on_event_violations: Arc<Mutex<Vec<String>>> = Arc::default();

    let p = {
        let want_user_content = want_user_content.clone();
        let cached = cached_reply.clone();
        let cached_for_event = cached_reply.clone();
        let want_reply = want_reply.clone();
        let (oec, arc, ran, preply, viol) = (
            on_event_calls.clone(),
            after_run_calls.clone(),
            ran_agent.clone(),
            plugin_reply.clone(),
            on_event_violations.clone(),
        );
        Plugin::new(gadk_agent::PluginConfig {
            name: "cache".into(),
            on_user_message_callback: Some(Arc::new(move |_ctx, _msg| {
                let c = want_user_content.clone();
                Box::pin(async move { Ok(Some(c)) })
            })),
            before_run_callback: Some(Arc::new(move |_ctx| {
                let out = if tc.before_err {
                    Err(AgentError::other("before run failed"))
                } else if tc.with_content {
                    Ok(Some(cached.clone()))
                } else {
                    Ok(None)
                };
                Box::pin(async move { out })
            })),
            before_agent_callback: Some(Arc::new(move |_ctx| {
                ran.store(true, Ordering::SeqCst);
                Box::pin(async { Err(AgentError::other("unexpected agent execution")) })
            })),
            on_event_callback: Some(Arc::new(move |ctx: InvocationContext, ev: Event| {
                oec.fetch_add(1, Ordering::SeqCst);
                let mut v = viol.lock();
                if ev.author != "assistant" {
                    v.push(format!("OnEvent author = {:?}", ev.author));
                }
                if ev.response.content.as_ref() != Some(&cached_for_event) {
                    v.push("OnEvent content mismatch".into());
                }
                let got = ctx.session().events().len();
                if got != want_user_events {
                    v.push(format!(
                        "history length before OnEvent = {got}, want {want_user_events}"
                    ));
                }
                drop(v);
                let out = if tc.on_event_err {
                    Err(AgentError::other("on event failed"))
                } else if tc.replace || tc.partial {
                    let mut modified = ev.clone();
                    modified.response.content = Some(want_reply.clone());
                    modified.response.partial = tc.partial;
                    modified.actions.state_delta = Some(obj(json!({"reply_processed": true})));
                    modified.response.custom_metadata = obj(json!({"cache": true}));
                    *preply.lock() = Some(modified.clone());
                    Ok(Some(modified))
                } else {
                    Ok(None)
                };
                Box::pin(async move { out })
            })),
            after_run_callback: Some(Arc::new(move |_ctx| {
                arc.fetch_add(1, Ordering::SeqCst);
                Box::pin(async {})
            })),
            ..Default::default()
        })
    };

    let svc = Arc::new(BeforeRunSessionService {
        inner: InMemorySessionService::new(),
        fail: tc.append_err.then_some("append failed"),
        reply_appends: AtomicUsize::new(0),
    });
    let runner = Runner::new(RunnerConfig {
        app_name: "test".into(),
        agent,
        session_service: svc.clone(),
        artifact_service: None,
        memory_service: None,
        plugin_config: PluginConfig {
            plugins: vec![p],
            close_timeout: Duration::from_secs(1),
        },
        auto_create_session: true,
        platform: Platform::default(),
    })
    .unwrap();

    let mut stream = runner.run(
        "user",
        "session",
        user_content,
        RunConfig::default(),
        RunOptions::default(),
    );
    let mut yield_count = 0;
    let mut last: Option<Result<Event, AgentError>> = None;
    while let Some(item) = stream.next().await {
        yield_count += 1;
        last = Some(item);
    }
    assert_eq!(yield_count, 1, "{label}: yield count");
    let violations = on_event_violations.lock().clone();
    assert!(violations.is_empty(), "{label}: {violations:?}");
    assert!(
        !ran_agent.load(Ordering::SeqCst),
        "{label}: agent execution was not short-circuited"
    );

    // Go errors.Is → the error text contains the Go error text.
    let want_err = if tc.before_err {
        Some("before run failed")
    } else if tc.on_event_err {
        Some("on event failed")
    } else if tc.append_err {
        Some("append failed")
    } else {
        None
    };
    let last = last.unwrap();
    let got_event = match (want_err, last) {
        (Some(want), Err(e)) => {
            assert!(
                e.to_string().contains(want),
                "{label}: error = {e}, want {want}"
            );
            None
        }
        (Some(want), Ok(_)) => panic!("{label}: error path yielded a content event, want {want}"),
        (None, Err(e)) => panic!("{label}: Run: {e}"),
        (None, Ok(ev)) => {
            assert_eq!(ev.author, "assistant", "{label}: reply author");
            assert_eq!(ev.response.partial, tc.partial, "{label}: reply partial");
            assert_eq!(
                ev.response.content.as_ref(),
                Some(&want_reply),
                "{label}: reply"
            );
            let mut want_metadata = obj(json!({BEFORE_RUN_REPLY_KEY: true}));
            if let Some(pr) = plugin_reply.lock().as_ref() {
                want_metadata.insert("cache".into(), Value::Bool(true));
                assert_eq!(
                    pr.response.custom_metadata,
                    obj(json!({"cache": true})),
                    "{label}: plugin-owned event metadata was mutated"
                );
            }
            assert_eq!(
                ev.response.custom_metadata, want_metadata,
                "{label}: reply metadata"
            );
            Some(ev)
        }
    };

    let want_on_event_calls = if tc.before_err { 0 } else { 1 };
    assert_eq!(
        on_event_calls.load(Ordering::SeqCst),
        want_on_event_calls,
        "{label}: OnEvent calls"
    );
    assert_eq!(
        after_run_calls.load(Ordering::SeqCst),
        1,
        "{label}: AfterRun calls"
    );

    let want_reply_appends = usize::from(!tc.before_err && !tc.on_event_err && !tc.partial);
    assert_eq!(
        svc.reply_appends.load(Ordering::SeqCst),
        want_reply_appends,
        "{label}: reply AppendEvent calls"
    );

    let fresh = svc
        .get(GetRequest {
            app_name: "test".into(),
            user_id: "user".into(),
            session_id: "session".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    let events = fresh.events();
    let want_stored = want_user_events + usize::from(want_err.is_none() && !tc.partial);
    assert_eq!(events.len(), want_stored, "{label}: stored events");
    if want_user_events > 0 {
        assert_eq!(events[0].author, "user", "{label}: user event author");
        assert_eq!(
            events[0].response.content.as_ref(),
            Some(&want_user_content),
            "{label}: stored user input"
        );
    }
    if want_stored > want_user_events {
        assert_eq!(
            got_event.as_ref(),
            Some(&events[want_user_events]),
            "{label}: stored reply"
        );
    }
    if tc.replace {
        assert_eq!(
            fresh.state().get("reply_processed").ok(),
            Some(Value::Bool(true)),
            "{label}: OnEvent state delta was not persisted"
        );
    }
}
