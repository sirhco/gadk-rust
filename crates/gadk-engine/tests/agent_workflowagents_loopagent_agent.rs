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
//! Port of Go agent/workflowagents/loopagent/agent_test.go.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use futures::stream::{self, BoxStream, StreamExt};
use gadk_agent::{
    Agent, AgentConfig, AgentError, Context, InvocationContext, RunConfig, ToolError,
};
use gadk_engine::agents::loop_agent::{self, LoopAgentConfig};
use gadk_engine::agents::sequential_agent::{self, SequentialAgentConfig};
use gadk_engine::llm::{LlmAgentConfig, agent as llm_agent};
use gadk_engine::runner::{PluginConfig, RunOptions, Runner, RunnerConfig};
use gadk_engine::tool::{FunctionTool, FunctionToolConfig};
use gadk_genai::{Content, Part, ROLE_MODEL, ROLE_USER};
use gadk_model::{Llm, LlmRequest, LlmResponse, ModelError};
use gadk_platform::Platform;
use gadk_session::{CreateRequest, Event, EventActions, InMemorySessionService, SessionService};
use pretty_assertions::assert_eq;
use serde_json::Map;

/// Go FakeLLM: a function call to `exampleFunction` on the first turn, then "hello <id>".
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
        req: LlmRequest,
        _stream: bool,
    ) -> BoxStream<'static, Result<LlmResponse, ModelError>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let content = if req.contents.len() == 1 {
            Content::from_parts(
                vec![Part::from_function_call("exampleFunction", Map::new())],
                ROLE_MODEL,
            )
        } else {
            Content::from_text(format!("hello {}", self.id), ROLE_MODEL)
        };
        stream::iter(vec![Ok(LlmResponse {
            content: Some(content),
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

/// Go newCustomAgent: a custom agent yielding one "hello <id>" event per run.
fn new_custom_agent(id: usize) -> Arc<dyn Agent> {
    gadk_agent::new_agent(AgentConfig {
        name: format!("custom_agent_{id}"),
        run: Some(Arc::new(move |_ctx: InvocationContext| {
            let mut ev = Event::default();
            ev.response.content = Some(Content::from_text(format!("hello {id}"), ROLE_MODEL));
            stream::iter(vec![Ok(ev)]).boxed()
        })),
        ..Default::default()
    })
    .unwrap()
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct EmptyArgs {}

/// Go newLmmAgentWithFunctionCall: an LLM agent whose `exampleFunction` tool escalates (and skips
/// summarization when asked).
fn new_llm_agent_with_function_call(id: usize, skip_summarization: bool) -> Arc<dyn Agent> {
    let tool = FunctionTool::new(
        FunctionToolConfig::new("exampleFunction", "Call this function to escalate\n"),
        move |ctx: Context, _args: EmptyArgs| async move {
            ctx.update_actions(|a| {
                a.escalate = true;
                a.skip_summarization = skip_summarization;
            });
            Ok::<HashMap<String, String>, ToolError>(HashMap::new())
        },
    )
    .unwrap_or_else(|e| panic!("error creating exampleFunction tool: {e}"));
    llm_agent::new(LlmAgentConfig {
        name: format!("custom_agent_{id}"),
        model: Some(Arc::new(FakeLlm {
            id,
            calls: AtomicUsize::new(0),
        })),
        tools: vec![tool],
        ..Default::default()
    })
    .unwrap()
}

fn runner_config(agent: Arc<dyn Agent>, auto_create_session: bool) -> RunnerConfig {
    RunnerConfig {
        app_name: "test_app".into(),
        agent,
        session_service: Arc::new(InMemorySessionService::new()),
        artifact_service: None,
        memory_service: None,
        plugin_config: PluginConfig::default(),
        auto_create_session,
        platform: Platform::default(),
    }
}

/// A custom-agent event as Go expects it: author + model text, no actions.
fn want_custom(id: usize) -> Event {
    let mut ev = Event {
        author: format!("custom_agent_{id}"),
        ..Default::default()
    };
    ev.response.content = Some(Content::from_text(format!("hello {id}"), ROLE_MODEL));
    ev
}

/// An LLM-agent event as Go expects it: empty (non-nil) state and artifact deltas.
fn want_llm(content: Content, actions: EventActions) -> Event {
    let mut ev = Event {
        author: "custom_agent_0".into(),
        ..Default::default()
    };
    ev.response.content = Some(content);
    ev.actions = EventActions {
        state_delta: Some(Map::new()),
        artifact_delta: Some(BTreeMap::new()),
        ..actions
    };
    ev
}

/// Go ignores Event.ID/InvocationID/Timestamp, FunctionCall.ID and FunctionResponse.ID.
fn normalize(mut ev: Event) -> Event {
    ev.id.clear();
    ev.invocation_id.clear();
    ev.timestamp = gadk_session::zero_time();
    if let Some(c) = &mut ev.response.content {
        for p in &mut c.parts {
            if let Some(fc) = &mut p.function_call {
                fc.id.clear();
            }
            if let Some(fr) = &mut p.function_response {
                fr.id.clear();
            }
        }
    }
    ev
}

struct NewCase {
    name: &'static str,
    max_iterations: u32,
    sub_agents: Vec<Arc<dyn Agent>>,
    want_events: Vec<Event>,
}

#[tokio::test]
async fn test_new_loop_agent() {
    let call = || {
        Content::from_parts(
            vec![Part::from_function_call("exampleFunction", Map::new())],
            ROLE_MODEL,
        )
    };
    let response = || {
        Content::from_parts(
            vec![Part::from_function_response("exampleFunction", Map::new())],
            ROLE_USER,
        )
    };
    let tests = vec![
        NewCase {
            name: "infinite loop",
            max_iterations: 0,
            sub_agents: vec![new_custom_agent(0)],
            want_events: vec![want_custom(0)],
        },
        NewCase {
            name: "loop agent with max iterations",
            max_iterations: 1,
            sub_agents: vec![new_custom_agent(0)],
            want_events: vec![want_custom(0)],
        },
        NewCase {
            name: "loop agent with max iterations and 2 sub agents",
            max_iterations: 1,
            sub_agents: vec![new_custom_agent(0), new_custom_agent(1)],
            want_events: vec![want_custom(0), want_custom(1)],
        },
        NewCase {
            name: "loop with escalate function returns summarization",
            max_iterations: 2,
            sub_agents: vec![
                new_llm_agent_with_function_call(0, false),
                new_custom_agent(1),
            ],
            want_events: vec![
                want_llm(call(), EventActions::default()),
                want_llm(
                    response(),
                    EventActions {
                        escalate: true,
                        ..Default::default()
                    },
                ),
                want_llm(
                    Content::from_text("hello 0", ROLE_MODEL),
                    EventActions::default(),
                ),
            ],
        },
        NewCase {
            name: "loop with escalate function returns summarization",
            max_iterations: 2,
            sub_agents: vec![
                new_llm_agent_with_function_call(0, true),
                new_custom_agent(1),
            ],
            want_events: vec![
                want_llm(call(), EventActions::default()),
                want_llm(
                    response(),
                    EventActions {
                        escalate: true,
                        skip_summarization: true,
                        ..Default::default()
                    },
                ),
            ],
        },
    ];

    for tt in tests {
        let name = tt.name;
        let loop_agent = loop_agent::new(LoopAgentConfig {
            agent: AgentConfig {
                name: "test_agent".into(),
                sub_agents: tt.sub_agents,
                ..Default::default()
            },
            max_iterations: tt.max_iterations,
        })
        .unwrap_or_else(|e| panic!("{name}: NewLoopAgent() error = {e}"));

        let session_service = Arc::new(InMemorySessionService::new());
        let agent_runner = Runner::new(RunnerConfig {
            session_service: session_service.clone(),
            ..runner_config(loop_agent, false)
        })
        .unwrap();
        session_service
            .create(CreateRequest {
                app_name: "test_app".into(),
                user_id: "user_id".into(),
                session_id: Some("session_id".into()),
                state: None,
            })
            .await
            .unwrap();

        let mut got_events = Vec::new();
        let mut s = agent_runner.run(
            "user_id",
            "session_id",
            Some(Content::from_text("user input", ROLE_USER)),
            RunConfig::default(),
            RunOptions::default(),
        );
        while let Some(item) = s.next().await {
            let event = item.unwrap_or_else(|e| panic!("{name}: got unexpected error: {e}"));
            if tt.max_iterations == 0 && got_events.len() == tt.want_events.len() {
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
        for (i, got) in got_events.into_iter().enumerate() {
            assert_eq!(
                tt.want_events[i],
                normalize(got),
                "{name}: event[{i}] mismatch"
            );
        }
    }
}

#[tokio::test]
async fn test_loop_agent_stops_on_sub_agent_error() {
    let llm = Arc::new(FailingLlm {
        calls: AtomicUsize::new(0),
    });
    let worker = llm_agent::new(LlmAgentConfig {
        name: "worker".into(),
        model: Some(llm.clone()),
        ..Default::default()
    })
    .unwrap();
    // A sub-agent after the failing one tells "stop now" apart from "finish this pass, then stop".
    let next_llm = Arc::new(FakeLlm {
        id: 1,
        calls: AtomicUsize::new(0),
    });
    let next = llm_agent::new(LlmAgentConfig {
        name: "next".into(),
        model: Some(next_llm.clone()),
        ..Default::default()
    })
    .unwrap();
    // No max_iterations: the loop is meant to end when a sub-agent escalates.
    let loop_agent = loop_agent::new(LoopAgentConfig {
        agent: AgentConfig {
            name: "loop".into(),
            sub_agents: vec![worker, next],
            ..Default::default()
        },
        max_iterations: 0,
    })
    .unwrap();
    let r = Runner::new(runner_config(loop_agent, true)).unwrap();

    // Keep reading after an error, and stop well before an unbounded loop would.
    const MAX_ERRORS: usize = 10;
    let mut err_count = 0;
    let mut s = r.run(
        "user_id",
        "session_id",
        Some(Content::from_text("go", ROLE_USER)),
        RunConfig::default(),
        RunOptions::default(),
    );
    while let Some(item) = s.next().await {
        if item.is_err() {
            err_count += 1;
            if err_count == MAX_ERRORS {
                break;
            }
        }
    }

    assert_eq!(
        err_count, 1,
        "the loop should end on the first sub-agent error"
    );
    assert_eq!(llm.calls.load(Ordering::SeqCst), 1, "model calls");
    assert_eq!(
        next_llm.calls.load(Ordering::SeqCst),
        0,
        "next agent's model called after worker failed"
    );
}

#[tokio::test]
async fn test_loop_agent_without_sub_agents_ends() {
    let loop_agent = loop_agent::new(LoopAgentConfig {
        agent: AgentConfig {
            name: "loop".into(),
            ..Default::default()
        },
        max_iterations: 0,
    })
    .unwrap();
    let r = Runner::new(runner_config(loop_agent, true)).unwrap();

    let drain = async {
        let mut events = Vec::new();
        let mut first_err = None;
        let mut s = r.run(
            "user_id",
            "session_id",
            Some(Content::from_text("go", ROLE_USER)),
            RunConfig::default(),
            RunOptions::default(),
        );
        while let Some(item) = s.next().await {
            match item {
                Ok(ev) => events.push(ev),
                Err(e) => {
                    first_err.get_or_insert(e);
                }
            }
        }
        (events, first_err)
    };
    let (events, first_err) = tokio::time::timeout(Duration::from_secs(2), drain)
        .await
        .expect("LoopAgent with no sub-agents and no MaxIterations did not end");
    assert!(
        first_err.is_none(),
        "LoopAgent with no sub-agents returned error {first_err:?}, want none"
    );
    assert_eq!(
        events.len(),
        0,
        "LoopAgent with no sub-agents yielded events"
    );
}

// parity: adapted: Runner::run takes no context; the loop agent runs directly on a built InvocationContext, deadline cases use Context::with_agent_timeout and cancel cases cancel the token after 50ms; a CancellationToken carries no cause, so both end in AgentError::Cancelled where Go's deadline cases expect DeadlineExceeded
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_loop_agent_stops_on_context_cancellation() {
    // An empty sequential agent yields no events, so nothing in the loop body returns control to
    // the consumer. Only the context can end the run.
    let empty = sequential_agent::new(SequentialAgentConfig {
        agent: AgentConfig {
            name: "empty".into(),
            ..Default::default()
        },
    })
    .unwrap();

    for (name, max_iterations, deadline) in [
        ("deadline with unlimited iterations", 0, true),
        ("deadline with MaxIterations set", u32::MAX, true),
        ("cancel with unlimited iterations", 0, false),
        ("cancel with MaxIterations set", u32::MAX, false),
    ] {
        let loop_agent = loop_agent::new(LoopAgentConfig {
            agent: AgentConfig {
                name: "loop".into(),
                sub_agents: vec![empty.clone()],
                ..Default::default()
            },
            max_iterations,
        })
        .unwrap();
        let ictx = InvocationContext::builder()
            .agent(loop_agent.clone())
            .build_invocation();

        let run = async {
            let mut got: Vec<Result<Event, AgentError>> = Vec::new();
            if deadline {
                let (c, _guard) = ictx
                    .clone()
                    .into_context()
                    .with_agent_timeout(Duration::from_millis(50));
                let mut s = loop_agent.run(c.as_invocation().clone());
                while let Some(item) = s.next().await {
                    got.push(item);
                }
            } else {
                let token = ictx.cancellation_token().clone();
                let canceller = tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    token.cancel();
                });
                let mut s = loop_agent.run(ictx.clone());
                while let Some(item) = s.next().await {
                    got.push(item);
                }
                canceller.abort();
            }
            got
        };
        let got = tokio::time::timeout(Duration::from_secs(5), run)
            .await
            .unwrap_or_else(|_| {
                panic!("{name}: loop agent did not stop after its context was canceled")
            });

        let (last, rest) = got
            .split_last()
            .unwrap_or_else(|| panic!("{name}: got no results, want a final context error"));
        for (i, res) in rest.iter().enumerate() {
            assert!(
                res.is_ok(),
                "{name}: result {i} has error {:?} before the end, want the context error last",
                res.as_ref().err()
            );
        }
        assert!(
            matches!(last, Err(AgentError::Cancelled)),
            "{name}: last result = {last:?}, want AgentError::Cancelled"
        );
    }
}
