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

//! Port of Go agent/workflowagents/sequentialagent/agent_test.go.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use futures::stream::{self, BoxStream, StreamExt};
use gadk_agent::{Agent, AgentConfig, RunConfig};
use gadk_genai::{Content, ROLE_MODEL, ROLE_USER};
use gadk_model::{Llm, LlmRequest, LlmResponse, ModelError};
use gadk_platform::Platform;
use gadk_session::{CreateRequest, Event, EventActions, InMemorySessionService, SessionService};
use pretty_assertions::assert_eq;
use serde_json::Map;

use crate::agents::sequential_agent::{self, SequentialAgentConfig};
use crate::llm::{LlmAgentConfig, agent as llm_agent};
use crate::runner::{PluginConfig, RunOptions, Runner, RunnerConfig};

/// Go FakeLLM: replies "hello <id>".
struct FakeLlm {
    id: usize,
    calls: AtomicUsize,
}

impl Llm for FakeLlm {
    fn name(&self) -> &str {
        "fake-llm"
    }
    fn generate_content(
        &self,
        _req: LlmRequest,
        _stream: bool,
    ) -> BoxStream<'static, Result<LlmResponse, ModelError>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        stream::iter(vec![Ok(LlmResponse {
            content: Some(Content::from_text(format!("hello {}", self.id), ROLE_MODEL)),
            ..Default::default()
        })])
        .boxed()
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// Go failingLLM: fails every call, as a model does on a quota or auth error.
struct FailingLlm {
    calls: AtomicUsize,
}

impl Llm for FailingLlm {
    fn name(&self) -> &str {
        "failing-llm"
    }
    fn generate_content(
        &self,
        _req: LlmRequest,
        _stream: bool,
    ) -> BoxStream<'static, Result<LlmResponse, ModelError>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        stream::iter(vec![Err(ModelError::Other("model unavailable".into()))]).boxed()
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// Go newCustomAgent.
fn new_custom_agent(id: usize) -> Arc<dyn Agent> {
    llm_agent::new(LlmAgentConfig {
        name: format!("custom_agent_{id}"),
        model: Some(Arc::new(FakeLlm {
            id,
            calls: AtomicUsize::new(0),
        })),
        ..Default::default()
    })
    .unwrap()
}

/// Go newSequentialAgent.
fn new_sequential_agent(sub_agents: Vec<Arc<dyn Agent>>, name: &str) -> Arc<dyn Agent> {
    sequential_agent::new(SequentialAgentConfig {
        agent: AgentConfig {
            name: name.into(),
            sub_agents,
            ..Default::default()
        },
    })
    .unwrap_or_else(|e| panic!("NewSequentialAgent() error = {e}"))
}

/// An expected LLM event: author + model text, empty (non-nil) state and artifact deltas.
fn want_event(id: usize) -> Event {
    let mut ev = Event {
        author: format!("custom_agent_{id}"),
        ..Default::default()
    };
    ev.response.content = Some(Content::from_text(format!("hello {id}"), ROLE_MODEL));
    ev.actions = EventActions {
        state_delta: Some(Map::new()),
        artifact_delta: Some(BTreeMap::new()),
        ..Default::default()
    };
    ev
}

/// Go cmpopts.IgnoreFields(session.Event{}, "ID", "Timestamp", "InvocationID").
fn normalize(mut ev: Event) -> Event {
    ev.id.clear();
    ev.invocation_id.clear();
    ev.timestamp = gadk_session::zero_time();
    ev
}

struct NewCase {
    name: &'static str,
    sub_agents: Vec<Arc<dyn Agent>>,
    want_events: Vec<Event>,
    want_err_message: Option<&'static str>,
}

#[tokio::test]
async fn test_new_sequential_agent() {
    let max_iterations = 0;
    let same_agent =
        new_sequential_agent(vec![new_custom_agent(1), new_custom_agent(2)], "same_agent");
    let tests = vec![
        NewCase {
            name: "ok",
            sub_agents: vec![new_custom_agent(0), new_custom_agent(1)],
            want_events: vec![want_event(0), want_event(1)],
            want_err_message: None,
        },
        NewCase {
            name: "ok with inner sequential",
            sub_agents: vec![
                new_custom_agent(0),
                new_sequential_agent(
                    vec![new_custom_agent(1), new_custom_agent(2)],
                    "test_agent1",
                ),
                new_custom_agent(3),
            ],
            want_events: vec![want_event(0), want_event(1), want_event(2), want_event(3)],
            want_err_message: None,
        },
        // As in Go, this case builds a valid tree and expects no events, so it passes vacuously.
        NewCase {
            name: "err with inner sequential with same name as root",
            sub_agents: vec![
                new_custom_agent(0),
                new_sequential_agent(
                    vec![new_custom_agent(1), new_custom_agent(2)],
                    "test_agent1",
                ),
                new_custom_agent(3),
            ],
            want_events: vec![],
            want_err_message: Some(
                r#"failed to create agent tree: agent names must be unique in the agent tree, found duplicate: "test_agent""#,
            ),
        },
        NewCase {
            name: "err with 2 levels of inner sequential with same name as root ",
            sub_agents: vec![
                new_custom_agent(0),
                new_sequential_agent(
                    vec![new_sequential_agent(
                        vec![new_custom_agent(1), new_custom_agent(2)],
                        "test_agent1",
                    )],
                    "test_agent",
                ),
                new_custom_agent(3),
            ],
            want_events: vec![],
            want_err_message: Some(
                r#"failed to create agent tree: agent names must be unique in the agent tree, found duplicate: "test_agent""#,
            ),
        },
        NewCase {
            name: "err with 2 levels of inner sequential with same name as parent ",
            sub_agents: vec![
                new_custom_agent(0),
                new_sequential_agent(
                    vec![new_sequential_agent(
                        vec![new_custom_agent(1), new_custom_agent(2)],
                        "test_agent1",
                    )],
                    "test_agent1",
                ),
                new_custom_agent(3),
            ],
            want_events: vec![],
            want_err_message: Some(
                r#"failed to create agent tree: agent names must be unique in the agent tree, found duplicate: "test_agent1""#,
            ),
        },
        NewCase {
            name: "err with repeated inner sequential",
            sub_agents: vec![
                new_custom_agent(0),
                same_agent.clone(),
                same_agent.clone(),
                new_custom_agent(3),
            ],
            want_events: vec![],
            want_err_message: Some(
                r#"failed to create base agent: error creating agent: subagent "same_agent" appears multiple times in subAgents"#,
            ),
        },
        NewCase {
            name: "err with repeated inner sequential in two levels",
            sub_agents: vec![
                new_custom_agent(0),
                new_sequential_agent(vec![same_agent.clone()], "test_agent1"),
                same_agent.clone(),
                new_custom_agent(3),
            ],
            want_events: vec![],
            want_err_message: Some(
                r#"failed to create agent tree: "same_agent" agent cannot have >1 parents, found: "test_agent1", "test_agent""#,
            ),
        },
    ];

    for tt in tests {
        let name = tt.name;
        let sequential = match sequential_agent::new(SequentialAgentConfig {
            agent: AgentConfig {
                name: "test_agent".into(),
                sub_agents: tt.sub_agents,
                ..Default::default()
            },
        }) {
            Ok(a) => a,
            Err(e) => {
                let want = tt
                    .want_err_message
                    .unwrap_or_else(|| panic!("{name}: NewSequentialAgent() error = {e}"));
                assert_eq!(want, e.to_string(), "{name}: err message mismatch");
                continue;
            }
        };

        let session_service = Arc::new(InMemorySessionService::new());
        let agent_runner = match Runner::new(RunnerConfig {
            app_name: "test_app".into(),
            agent: sequential,
            session_service: session_service.clone(),
            artifact_service: None,
            memory_service: None,
            plugin_config: PluginConfig::default(),
            auto_create_session: false,
            platform: Platform::default(),
        }) {
            Ok(r) => r,
            Err(e) => {
                let want = tt
                    .want_err_message
                    .unwrap_or_else(|| panic!("{name}: NewSequentialAgent() error = {e}"));
                assert_eq!(want, e.to_string(), "{name}: err message mismatch");
                continue;
            }
        };

        session_service
            .create(CreateRequest {
                app_name: "test_app".into(),
                user_id: "user_id".into(),
                session_id: Some("session_id".into()),
                state: None,
            })
            .await
            .unwrap();

        // Run twice: the second time the runner must determine which agent to use, and the result
        // must be the same. As in Go, the collected events carry over between the two runs.
        let mut got_events: Vec<Event> = Vec::new();
        for _ in 0..2 {
            let mut s = agent_runner.run(
                "user_id",
                "session_id",
                Some(Content::from_text("user input", ROLE_USER)),
                RunConfig::default(),
                RunOptions::default(),
            );
            while let Some(item) = s.next().await {
                let event = item.unwrap_or_else(|e| panic!("{name}: got unexpected error: {e}"));
                if max_iterations == 0 && got_events.len() == tt.want_events.len() {
                    break;
                }
                got_events.push(event);
            }
            drop(s);

            assert_eq!(
                got_events.len(),
                tt.want_events.len(),
                "{name}: unexpected event length"
            );
            for (i, got) in got_events.iter().enumerate() {
                assert_eq!(
                    tt.want_events[i],
                    normalize(got.clone()),
                    "{name}: event[{i}] mismatch"
                );
            }
        }
    }
}

// parity: skipped TestSequentialAgent_RunLive_Injection: live mode (RunLive) is deferred to 1b-live
// parity: skipped TestSequentialAgent_RunLive_NestedSequentialInjection: live mode (RunLive) is deferred to 1b-live
// parity: skipped TestSequentialAgent_RunLive_NonLiveCompositeNotMutated: live mode (RunLive) is deferred to 1b-live
// parity: skipped TestSequentialAgent_RunLive_SequentialOrchestration: live mode (RunLive) is deferred to 1b-live
// parity: skipped TestSequentialAgent_RunLive_NestedSequentialOrchestration: live mode (RunLive) is deferred to 1b-live

#[tokio::test]
async fn test_sequential_agent_stops_on_sub_agent_error() {
    let first_llm = Arc::new(FailingLlm {
        calls: AtomicUsize::new(0),
    });
    let first = llm_agent::new(LlmAgentConfig {
        name: "first".into(),
        model: Some(first_llm.clone()),
        ..Default::default()
    })
    .unwrap();
    let second_llm = Arc::new(FakeLlm {
        id: 1,
        calls: AtomicUsize::new(0),
    });
    let second = llm_agent::new(LlmAgentConfig {
        name: "second".into(),
        model: Some(second_llm.clone()),
        ..Default::default()
    })
    .unwrap();
    let r = Runner::new(RunnerConfig {
        app_name: "test_app".into(),
        agent: new_sequential_agent(vec![first, second], "pipeline"),
        session_service: Arc::new(InMemorySessionService::new()),
        artifact_service: None,
        memory_service: None,
        plugin_config: PluginConfig::default(),
        auto_create_session: true,
        platform: Platform::default(),
    })
    .unwrap();

    // Keep reading after an error, as the run_sse handler and the console launcher do.
    let mut errs = Vec::new();
    let mut s = r.run(
        "user_id",
        "session_id",
        Some(Content::from_text("go", ROLE_USER)),
        RunConfig::default(),
        RunOptions::default(),
    );
    while let Some(item) = s.next().await {
        if let Err(e) = item {
            errs.push(e.to_string());
        }
    }

    assert!(
        errs.len() == 1 && errs[0].contains("model unavailable"),
        "got errors {errs:?}, want exactly one from the first agent's model"
    );
    assert_eq!(
        first_llm.calls.load(Ordering::SeqCst),
        1,
        "first agent's model calls"
    );
    assert_eq!(
        second_llm.calls.load(Ordering::SeqCst),
        0,
        "second agent's model called after the first agent failed"
    );
}
