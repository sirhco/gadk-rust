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
//! Port of Go agent/workflowagents/parallelagent/agent_test.go.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use futures::StreamExt;
use futures::stream;
use gadk_agent::{
    AfterAgentCallback, Agent, AgentConfig, AgentError, Context, EventStream, InvocationContext,
    RunConfig,
};
use gadk_engine::agents::loop_agent::{self, LoopAgentConfig};
use gadk_engine::agents::parallel_agent::{self, ParallelAgentConfig};
use gadk_engine::llm::{LlmAgentConfig, agent as llm_agent};
use gadk_engine::runner::{PluginConfig, RunOptions, Runner, RunnerConfig};
use gadk_engine::testutil::{MockModel, TestAgentRunner, collect_events, gemini_test_model};
use gadk_engine::tool::{FunctionTool, FunctionToolConfig};
use gadk_genai::{Content, ROLE_MODEL, ROLE_USER};
use gadk_platform::Platform;
use gadk_session::{CreateRequest, Event, GetRequest, InMemorySessionService, SessionService};
use parking_lot::Mutex;
use pretty_assertions::assert_eq;
use serde::Deserialize;
use serde_json::{Map, Value, json};

const MODEL_NAME: &str = "gemini-2.5-flash";

fn model_event(text: &str) -> Event {
    let mut ev = Event::default();
    ev.response.content = Some(Content::from_text(text, ROLE_MODEL));
    ev
}

fn runner_config(
    agent: Arc<dyn Agent>,
    session_service: Arc<dyn SessionService>,
    auto_create_session: bool,
) -> RunnerConfig {
    RunnerConfig {
        app_name: "test_app".into(),
        agent,
        session_service,
        artifact_service: None,
        memory_service: None,
        plugin_config: PluginConfig::default(),
        auto_create_session,
        platform: Platform::default(),
        compaction: None,
    }
}

async fn create_session(svc: &Arc<InMemorySessionService>) {
    svc.create(CreateRequest {
        app_name: "test_app".into(),
        user_id: "user_id".into(),
        session_id: Some("session_id".into()),
        state: None,
    })
    .await
    .unwrap();
}

fn run_user_input(r: &Runner, text: &str) -> EventStream {
    r.run(
        "user_id",
        "session_id",
        Some(Content::from_text(text, ROLE_USER)),
        RunConfig::default(),
        RunOptions::default(),
    )
}

/// Go customRun: sleeps 1–5ms, then yields "hello <id>" (or `agent_err`).
fn custom_run(id: i64, agent_err: Option<&'static str>) -> gadk_agent::AgentRunFn {
    Arc::new(move |_ctx: InvocationContext| {
        stream::once(async move {
            tokio::time::sleep(Duration::from_millis((id.rem_euclid(5) + 1) as u64)).await;
            match agent_err {
                Some(e) => Err(AgentError::other(e)),
                None => Ok(model_event(&format!("hello {id}"))),
            }
        })
        .boxed()
    })
}

/// Go newParallelAgent: `n` loop agents (each running a custom `sub<i>` for `max_iterations`
/// passes, forever with 0), plus an `error_agent` when `agent_err` is set.
fn new_parallel_agent(
    max_iterations: u32,
    n: i64,
    agent_err: Option<&'static str>,
) -> Arc<dyn Agent> {
    let mut sub_agents = Vec::new();
    for i in 1..=n {
        let sub = gadk_agent::new_agent(AgentConfig {
            name: format!("sub{i}"),
            run: Some(custom_run(i, None)),
            ..Default::default()
        })
        .unwrap();
        sub_agents.push(
            loop_agent::new(LoopAgentConfig {
                agent: AgentConfig {
                    name: format!("loop_agent_{i}"),
                    sub_agents: vec![sub],
                    ..Default::default()
                },
                max_iterations,
            })
            .unwrap(),
        );
    }
    if agent_err.is_some() {
        sub_agents.push(
            gadk_agent::new_agent(AgentConfig {
                name: "error_agent".into(),
                run: Some(custom_run(-1, agent_err)),
                ..Default::default()
            })
            .unwrap(),
        );
    }
    parallel_agent::new(ParallelAgentConfig {
        agent: AgentConfig {
            name: "test_agent".into(),
            sub_agents,
            ..Default::default()
        },
    })
    .unwrap()
}

async fn run_new_parallel_case(
    max_iterations: u32,
    n: i64,
    agent_err: Option<&'static str>,
) -> Vec<Result<Event, AgentError>> {
    let svc = Arc::new(InMemorySessionService::new());
    let r = Runner::new(runner_config(
        new_parallel_agent(max_iterations, n, agent_err),
        svc.clone(),
        false,
    ))
    .unwrap();
    create_session(&svc).await;
    let s = run_user_input(&r, "user input");
    tokio::time::timeout(Duration::from_secs(10), s.collect::<Vec<_>>())
        .await
        .expect("run did not end")
}

// parity: adapted: Go's random 1–5ms sleep is a deterministic (id % 5 + 1)ms tokio sleep; the "handle ctx cancel" case has zero sub-agents in Go, so it only checks termination (Runner::run takes no context)
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_new_parallel_agent() {
    // subagents complete run
    let got = run_new_parallel_case(2, 3, None).await;
    let mut got_events: Vec<Event> = got
        .into_iter()
        .map(|r| r.unwrap_or_else(|e| panic!("subagents complete run: unexpected error: {e}")))
        .map(|mut ev| {
            // IDs and timestamps are filled in when the events are run and appended, so they
            // cannot be expressed in a fixture (Go ignores ID and Timestamp).
            ev.id.clear();
            ev.timestamp = Event::default().timestamp;
            ev
        })
        .collect();
    let mut want_events = Vec::new();
    for agent_id in 1..=3 {
        for _ in 1..=2 {
            let mut ev = model_event(&format!("hello {agent_id}"));
            ev.author = format!("sub{agent_id}");
            want_events.push(ev);
        }
    }
    got_events.sort_by(|a, b| a.author.cmp(&b.author));
    assert_eq!(want_events, got_events, "subagents complete run: events");

    // handle ctx cancel: zero sub-agents, so the run simply ends.
    let got = run_new_parallel_case(0, 0, None).await;
    assert!(got.is_empty(), "handle ctx cancel: got {} items", got.len());

    // agent returns error: one agent fails while the others loop forever.
    let got = run_new_parallel_case(0, 100, Some("agent error")).await;
    assert!(
        got.iter().any(|r| r
            .as_ref()
            .is_err_and(|e| e.to_string().contains("agent error"))),
        "agent returns error: no error item"
    );
}

/// Increments the counter when the sub-agent's stream is torn down (Go's deferred teardown).
struct TeardownGuard(Arc<AtomicUsize>);

impl Drop for TeardownGuard {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

// parity: adapted: a dropped Rust stream cannot block its dropper; asserts every sub-agent stream is torn down (its drop guard runs) after the consumer drops the stream (waiting first until all three sub-agent runs have started, as Go's goroutines always do)
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_parallel_agent_awaits_sub_agent_teardown_on_early_stop() {
    const NUM_SUB_AGENTS: usize = 3;
    let started = Arc::new(AtomicUsize::new(0));
    let teardown_finished = Arc::new(AtomicUsize::new(0));

    let mut sub_agents = Vec::new();
    for i in 1..=NUM_SUB_AGENTS {
        let (started, finished) = (started.clone(), teardown_finished.clone());
        sub_agents.push(
            gadk_agent::new_agent(AgentConfig {
                name: format!("sub{i}"),
                run: Some(Arc::new(move |_ctx: InvocationContext| {
                    started.fetch_add(1, Ordering::SeqCst);
                    let guard = TeardownGuard(finished.clone());
                    Box::pin(async_stream::stream! {
                        let _guard = guard;
                        loop {
                            yield Ok(model_event(&format!("hello {i}")));
                        }
                    })
                })),
                ..Default::default()
            })
            .unwrap(),
        );
    }
    let parallel = parallel_agent::new(ParallelAgentConfig {
        agent: AgentConfig {
            name: "test_agent".into(),
            sub_agents,
            ..Default::default()
        },
    })
    .unwrap();
    let r = Runner::new(runner_config(
        parallel,
        Arc::new(InMemorySessionService::new()),
        true,
    ))
    .unwrap();

    let mut s = run_user_input(&r, "user input");
    let first = s.next().await.expect("an event");
    assert!(first.is_ok(), "Run() yielded error: {:?}", first.err());

    let wait_for = |counter: Arc<AtomicUsize>, what: &'static str| async move {
        tokio::time::timeout(Duration::from_secs(5), async {
            while counter.load(Ordering::SeqCst) < NUM_SUB_AGENTS {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "{what} = {}, want {NUM_SUB_AGENTS}",
                counter.load(Ordering::SeqCst)
            )
        });
    };
    wait_for(started.clone(), "started").await;
    // Stop after the first event.
    drop(s);
    wait_for(teardown_finished.clone(), "teardownFinished").await;
    assert_eq!(teardown_finished.load(Ordering::SeqCst), NUM_SUB_AGENTS);
}

#[derive(Deserialize, schemars::JsonSchema)]
struct SearchArgs {
    #[serde(rename = "Query")]
    query: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct AnalyzeArgs {
    #[serde(rename = "Data")]
    data: String,
}

/// Go args schema `struct{ X string }` as the fixture's request carries it.
fn args_schema(field: &str) -> Value {
    json!({"additionalProperties": false, "properties": {field: {"type": "string"}}, "required": [field], "type": "object"})
}

/// Go createAgentWithGemini.
async fn create_agent_with_gemini(name: &str) -> Arc<dyn Agent> {
    let search_name = name.to_string();
    let search_tool = FunctionTool::new(
        FunctionToolConfig {
            input_schema: Some(args_schema("Query")),
            output_schema: Some(json!({"type": "string"})),
            ..FunctionToolConfig::new(
                format!("search_tool_{name}"),
                "Search for information on the web",
            )
        },
        move |_ctx: Context, args: SearchArgs| {
            let name = search_name.clone();
            async move {
                Ok::<_, gadk_agent::ToolError>(format!(
                    "search result for '{}' from {name}",
                    args.query
                ))
            }
        },
    )
    .unwrap_or_else(|e| panic!("Failed to create search tool: {e}"));
    let analyze_name = name.to_string();
    let analyze_tool = FunctionTool::new(
        FunctionToolConfig {
            input_schema: Some(args_schema("Data")),
            output_schema: Some(json!({"type": "string"})),
            ..FunctionToolConfig::new(
                format!("analyze_tool_{name}"),
                "Analyze data and return insights",
            )
        },
        move |_ctx: Context, args: AnalyzeArgs| {
            let name = analyze_name.clone();
            async move {
                Ok::<_, gadk_agent::ToolError>(format!(
                    "analysis result for '{}' from {name}",
                    args.data
                ))
            }
        },
    )
    .unwrap_or_else(|e| panic!("Failed to create analyze tool: {e}"));

    let fixture = format!(
        "{}/tests/testdata/agent/workflowagents/parallelagent/testdata/TestParallelAgentWithTools_{name}.httprr",
        env!("CARGO_MANIFEST_DIR")
    );
    llm_agent::new(LlmAgentConfig {
        name: name.into(),
        description: format!("Test agent {name} that searches for information"),
        model: Some(gemini_test_model(MODEL_NAME, fixture).await),
        tools: vec![search_tool, analyze_tool],
        instruction: "Use the search tool to find information, then provide a brief response."
            .into(),
        ..Default::default()
    })
    .unwrap_or_else(|e| panic!("Failed to create agent {name}: {e}"))
}

#[tokio::test]
async fn test_parallel_agent_with_tools() {
    let agent1 = create_agent_with_gemini("agent1").await;
    let agent2 = create_agent_with_gemini("agent2").await;
    let parallel = parallel_agent::new(ParallelAgentConfig {
        agent: AgentConfig {
            name: "parallel_test".into(),
            sub_agents: vec![agent1, agent2],
            ..Default::default()
        },
    })
    .unwrap_or_else(|e| panic!("Failed to create parallel agent: {e}"));

    let runner = TestAgentRunner::new(parallel).await;
    let events = collect_events(runner.run("Search for AI news"))
        .await
        .unwrap_or_else(|e| panic!("Agent run failed: {e}"));
    assert!(
        events.len() >= 2,
        "Expected at least 2 events from parallel agents, got {}",
        events.len()
    );

    // Count function calls and responses per branch.
    let mut branch_calls: HashMap<String, usize> = HashMap::new();
    let mut branch_responses: HashMap<String, usize> = HashMap::new();
    for ev in &events {
        if let Some(c) = &ev.response.content {
            for part in &c.parts {
                if part.function_call.is_some() {
                    *branch_calls.entry(ev.branch.clone()).or_default() += 1;
                }
                if part.function_response.is_some() {
                    *branch_responses.entry(ev.branch.clone()).or_default() += 1;
                }
            }
        }
    }
    for (branch, calls) in &branch_calls {
        let responses = branch_responses.get(branch).copied().unwrap_or(0);
        assert!(
            *calls <= responses,
            "Branch {branch}: session has {calls} FunctionCalls but only {responses} FunctionResponses. \
             This indicates race condition: agent read session before FunctionResponse was appended."
        );
    }
}

// parity: adapted: runs the parallel agent directly on a built InvocationContext and cancels its token (Runner::run takes no context)
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_parallel_agent_propagates_context_error() {
    // A sub-agent that yields an event and then waits for cancellation.
    let sub_agent = gadk_agent::new_agent(AgentConfig {
        name: "yielder".into(),
        run: Some(Arc::new(|ctx: InvocationContext| {
            Box::pin(async_stream::stream! {
                yield Ok(model_event("hello"));
                ctx.cancellation_token().cancelled().await;
            })
        })),
        ..Default::default()
    })
    .unwrap();
    let parallel = parallel_agent::new(ParallelAgentConfig {
        agent: AgentConfig {
            name: "parallel_agent".into(),
            sub_agents: vec![sub_agent],
            ..Default::default()
        },
    })
    .unwrap();

    let ictx = InvocationContext::builder()
        .agent(parallel.clone())
        .build_invocation();
    let token = ictx.cancellation_token().clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(10)).await;
        token.cancel();
    });

    let mut yielded_error = None;
    let mut s = parallel.run(ictx);
    let drain = async {
        while let Some(item) = s.next().await {
            if let Err(e) = item {
                yielded_error = Some(e.to_string());
            }
            // Simulate processing delay so the sub-agent is blocked on its acknowledgement when
            // the cancellation happens.
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(5), drain)
        .await
        .expect("parallel agent did not end");
    assert_eq!(
        yielded_error.as_deref(),
        Some(r#"failed to run sub-agent "yielder": context canceled"#),
        "expected parallelAgent to yield the context error"
    );
}

#[tokio::test]
async fn test_parallel_agent_state_sync() {
    let got: Arc<Mutex<Option<Result<Value, String>>>> = Arc::new(Mutex::new(None));
    let got2 = got.clone();
    let after: AfterAgentCallback = Arc::new(move |c: Context| {
        let got = got2.clone();
        Box::pin(async move {
            *got.lock() = Some(c.state().get("test_key").map_err(|e| e.to_string()));
            Ok(None)
        })
    });
    let sub_agent = gadk_agent::new_agent(AgentConfig {
        name: "test_subagent".into(),
        run: Some(Arc::new(|_ctx: InvocationContext| {
            let mut ev = model_event("hello");
            let mut delta = Map::new();
            delta.insert("test_key".into(), json!("test_value"));
            ev.actions.state_delta = Some(delta);
            stream::iter(vec![Ok(ev)]).boxed()
        })),
        after_agent_callbacks: vec![after],
        ..Default::default()
    })
    .unwrap();
    let parallel = parallel_agent::new(ParallelAgentConfig {
        agent: AgentConfig {
            name: "test_parallel_agent".into(),
            sub_agents: vec![sub_agent],
            ..Default::default()
        },
    })
    .unwrap();

    let svc = Arc::new(InMemorySessionService::new());
    let r = Runner::new(runner_config(parallel, svc.clone(), false)).unwrap();
    create_session(&svc).await;

    let mut s = run_user_input(&r, "user input");
    while let Some(item) = s.next().await {
        item.unwrap_or_else(|e| panic!("{e}"));
    }

    let got = got.lock().clone();
    match got {
        Some(Ok(v)) => assert_eq!(v, json!("test_value"), "state value"),
        Some(Err(e)) => panic!("expected to get value from state, got error: {e}"),
        None => panic!("after-agent callback did not run"),
    }
}

/// Every sub-agent runs with the parent's isolation scope. A workflow node that wraps a parallel
/// agent stamps the node's scope on every event the branches emit, and the LLM history filter
/// keeps only events whose scope matches exactly. A branch that ran unscoped would therefore drop
/// its own earlier turns from its prompt.
#[tokio::test]
async fn test_parallel_agent_propagates_isolation_scope() {
    const SCOPE: &str = "scope-1";
    let got_scopes: Arc<Mutex<HashMap<String, String>>> = Arc::default();
    let got_branch: Arc<Mutex<HashMap<String, String>>> = Arc::default();
    let record = |name: &str| {
        let (scopes, branches, name2) = (got_scopes.clone(), got_branch.clone(), name.to_string());
        gadk_agent::new_agent(AgentConfig {
            name: name.into(),
            run: Some(Arc::new(move |ctx: InvocationContext| {
                scopes.lock().insert(name2.clone(), ctx.isolation_scope());
                branches.lock().insert(name2.clone(), ctx.branch());
                stream::empty().boxed()
            })),
            ..Default::default()
        })
        .unwrap()
    };
    let parallel = parallel_agent::new(ParallelAgentConfig {
        agent: AgentConfig {
            name: "parallel".into(),
            sub_agents: vec![record("a"), record("b")],
            ..Default::default()
        },
    })
    .unwrap();

    let svc = InMemorySessionService::new();
    let created = svc
        .create(CreateRequest {
            app_name: "app".into(),
            user_id: "user".into(),
            session_id: Some("session".into()),
            state: None,
        })
        .await
        .unwrap();
    let ctx = InvocationContext::builder()
        .session(created)
        .agent(parallel.clone())
        .isolation_scope(SCOPE)
        .invocation_id("inv-1")
        .build_invocation();

    let mut s = parallel.run(ctx);
    while let Some(item) = s.next().await {
        if let Err(e) = item {
            panic!("{e}");
        }
    }

    let want_scopes: HashMap<String, String> = [
        ("a".to_string(), SCOPE.to_string()),
        ("b".to_string(), SCOPE.to_string()),
    ]
    .into();
    assert_eq!(
        want_scopes,
        *got_scopes.lock(),
        "sub-agent isolation scopes"
    );
    let want_branch: HashMap<String, String> = [
        ("a".to_string(), "parallel.a".to_string()),
        ("b".to_string(), "parallel.b".to_string()),
    ]
    .into();
    assert_eq!(want_branch, *got_branch.lock(), "sub-agent branches");
}

/// The user-visible effect: an LLM branch of a scoped parallel agent must see its own earlier,
/// scoped turn in the prompt it sends to the model.
#[tokio::test]
async fn test_parallel_agent_scoped_branch_sees_its_own_history() {
    const SCOPE: &str = "scope-1";
    let mock = MockModel::from_contents(vec![Content::from_text("done", ROLE_MODEL)]);
    let branch_agent = llm_agent::new(LlmAgentConfig {
        name: "a".into(),
        model: Some(mock.clone()),
        ..Default::default()
    })
    .unwrap();
    let parallel = parallel_agent::new(ParallelAgentConfig {
        agent: AgentConfig {
            name: "parallel".into(),
            sub_agents: vec![branch_agent],
            ..Default::default()
        },
    })
    .unwrap();

    let svc = InMemorySessionService::new();
    let created = svc
        .create(CreateRequest {
            app_name: "app".into(),
            user_id: "user".into(),
            session_id: Some("session".into()),
            state: None,
        })
        .await
        .unwrap();
    // An earlier turn of this branch, stamped with the node's scope the way workflow's agent
    // node stamps every event a wrapped agent emits.
    let mut earlier = model_event("earlier scoped answer");
    earlier.id = "earlier".into();
    earlier.invocation_id = "inv-1".into();
    earlier.author = "a".into();
    earlier.branch = "parallel.a".into();
    earlier.isolation_scope = SCOPE.into();
    svc.append_event(&*created, &mut earlier).await.unwrap();
    let got = svc
        .get(GetRequest {
            app_name: "app".into(),
            user_id: "user".into(),
            session_id: "session".into(),
            ..Default::default()
        })
        .await
        .unwrap();

    let ctx = InvocationContext::builder()
        .session(got)
        .agent(parallel.clone())
        .isolation_scope(SCOPE)
        .invocation_id("inv-1")
        .run_config(RunConfig::default())
        .build_invocation();
    let mut s = parallel.run(ctx);
    while let Some(item) = s.next().await {
        if let Err(e) = item {
            panic!("{e}");
        }
    }

    let requests = mock.requests();
    assert_eq!(requests.len(), 1, "model requests");
    let texts: Vec<String> = requests[0]
        .contents
        .iter()
        .flat_map(|c| c.parts.iter().map(|p| p.text.clone()))
        .collect();
    assert!(
        texts.iter().any(|t| t == "earlier scoped answer"),
        "branch prompt is missing its own scoped history; prompt texts: {texts:?}"
    );
}
