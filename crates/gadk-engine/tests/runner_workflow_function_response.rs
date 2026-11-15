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
//! Port of ADK-Go runner/workflow_function_response_test.go.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering::SeqCst};
use std::time::Duration;

use futures::StreamExt;
use gadk_agent::{
    Agent, AgentConfig, AgentError, AgentRunFn, Context, EventStream, InvocationContext, RunConfig,
    ToolError,
};
use gadk_engine::agents::loop_agent::{self, LoopAgentConfig};
use gadk_engine::agents::parallel_agent::{self, ParallelAgentConfig};
use gadk_engine::agents::sequential_agent::{self, SequentialAgentConfig};
use gadk_engine::agents::workflow_agent::{self, WorkflowAgentConfig};
use gadk_engine::llm::LlmAgentConfig;
use gadk_engine::llm::agent as llm_agent;
use gadk_engine::runner::{PluginConfig, RunOptions, Runner, RunnerConfig};
use gadk_engine::testutil::MockModel;
use gadk_engine::tool::{FunctionTool, FunctionToolConfig};
use gadk_engine::workflow::{Edge, Node, START, WORKFLOW_INPUT_FUNCTION_CALL_NAME};
use gadk_genai::{Content, FunctionCall, FunctionResponse, Part};
use gadk_model::Llm;
use gadk_platform::Platform;
use gadk_session::{CreateRequest, Event, InMemorySessionService, SessionService};
use serde_json::{Map, Value, json};

mod workflow_support;
use workflow_support::{agent_node, join_resume_agent, resume_content};

fn start() -> Arc<dyn Node> {
    Arc::clone(&START)
}

fn edge(from: &Arc<dyn Node>, to: &Arc<dyn Node>) -> Edge {
    Edge {
        from: from.clone(),
        to: to.clone(),
        route: None,
    }
}

fn user_text(text: &str) -> Content {
    Content::from_text(text, "user")
}

fn model_text(text: &str) -> Content {
    Content::from_text(text, "model")
}

/// Go `for ev, err := range r.Run(...) { if err != nil { t.Fatal(err) } }`: every event.
async fn drain(s: EventStream) -> Vec<Event> {
    futures::TryStreamExt::try_collect(s)
        .await
        .expect("runner yielded error")
}

fn runner_over(agent: Arc<dyn Agent>, app: &str, svc: Arc<dyn SessionService>) -> Runner {
    Runner::new(RunnerConfig {
        app_name: app.into(),
        agent,
        session_service: svc,
        artifact_service: None,
        memory_service: None,
        plugin_config: PluginConfig::default(),
        auto_create_session: true,
        platform: Platform::default(),
        compaction: None,
    })
    .expect("runner.New()")
}

fn run(r: &Runner, session_id: &str, msg: Content) -> EventStream {
    r.run(
        "u",
        session_id,
        Some(msg),
        RunConfig::default(),
        RunOptions::default(),
    )
}

fn sequential(name: &str, subs: Vec<Arc<dyn Agent>>) -> Arc<dyn Agent> {
    sequential_agent::new(SequentialAgentConfig {
        agent: AgentConfig {
            name: name.into(),
            sub_agents: subs,
            ..Default::default()
        },
    })
    .expect("sequentialagent.New()")
}

fn parallel(name: &str, subs: Vec<Arc<dyn Agent>>) -> Arc<dyn Agent> {
    parallel_agent::new(ParallelAgentConfig {
        agent: AgentConfig {
            name: name.into(),
            sub_agents: subs,
            ..Default::default()
        },
    })
    .expect("parallelagent.New()")
}

fn looped(name: &str, max_iterations: u32, subs: Vec<Arc<dyn Agent>>) -> Arc<dyn Agent> {
    loop_agent::new(LoopAgentConfig {
        agent: AgentConfig {
            name: name.into(),
            sub_agents: subs,
            ..Default::default()
        },
        max_iterations,
    })
    .expect("loopagent.New()")
}

/// Go `workflowagent.New` over START → first → second, listing both agents as sub-agents.
fn two_node_workflow(name: &str, first: Arc<dyn Agent>, second: Arc<dyn Agent>) -> Arc<dyn Agent> {
    let (fnode, snode) = (agent_node(&first), agent_node(&second));
    workflow_agent::new(WorkflowAgentConfig {
        name: name.into(),
        sub_agents: vec![first, second],
        edges: vec![edge(&start(), &fnode), edge(&fnode, &snode)],
        ..Default::default()
    })
    .expect("workflowagent.New()")
}

/// Go `pausedWorkflow`: a graph workflow whose node `ask` asks for input and whose node `done`
/// runs after the answer.
fn paused_workflow(name: &str, ask: &str, done: &str) -> Arc<dyn Agent> {
    two_node_workflow(
        name,
        join_resume_agent(ask, "ASK", true),
        join_resume_agent(done, "DONE", false),
    )
}

type Emitter = Arc<dyn Fn(&InvocationContext, &mut Event) + Send + Sync>;

/// Go `customAgent`: an agent that runs `emit` each time it runs and counts the runs in `runs`.
fn custom_agent(name: &str, runs: Arc<AtomicI32>, emit: Option<Emitter>) -> Arc<dyn Agent> {
    let agent_name = name.to_string();
    let run_fn: AgentRunFn = Arc::new(move |ctx: InvocationContext| -> EventStream {
        runs.fetch_add(1, SeqCst);
        let mut ev = Event::new(ctx.platform(), ctx.invocation_id());
        ev.response.content = Some(model_text(&agent_name));
        if let Some(emit) = &emit {
            emit(&ctx, &mut ev);
        }
        Box::pin(futures::stream::iter([Ok(ev)]))
    });
    gadk_agent::agent::new(AgentConfig {
        name: name.into(),
        run: Some(run_fn),
        ..Default::default()
    })
    .expect("agent.New")
}

/// Go `runResumeTurn`: a first turn, then a turn with `msg`; how many events each author yielded
/// on the second turn. `on_resume` runs between the two.
async fn run_resume_turn(
    root: Arc<dyn Agent>,
    msg: Content,
    on_resume: impl FnOnce(),
) -> (HashMap<String, i32>, Option<AgentError>) {
    let r = runner_over(root, "repro", Arc::new(InMemorySessionService::new()));
    drain(run(&r, "s", user_text("start"))).await;
    on_resume();
    let mut authors: HashMap<String, i32> = HashMap::new();
    let mut s = run(&r, "s", msg);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        match tokio::time::timeout_at(deadline, s.next()).await {
            Err(_) => {
                return (
                    authors,
                    Some(AgentError::other("context deadline exceeded")),
                );
            }
            Ok(None) => return (authors, None),
            Ok(Some(Err(e))) => return (authors, Some(e)),
            Ok(Some(Ok(ev))) => *authors.entry(ev.author).or_default() += 1,
        }
    }
}

fn has_function_response(contents: &[Content], call_id: &str) -> bool {
    contents.iter().any(|c| {
        c.parts.iter().any(|p| {
            p.function_response
                .as_ref()
                .is_some_and(|fr| fr.id == call_id)
        })
    })
}

fn content_texts(contents: &[Content]) -> Vec<String> {
    contents
        .iter()
        .flat_map(|c| c.parts.iter())
        .filter(|p| !p.text.is_empty())
        .map(|p| p.text.clone())
        .collect()
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct NoArgs {}

type RootOf2 = fn(Arc<dyn Agent>, Arc<dyn Agent>) -> Arc<dyn Agent>;

#[tokio::test]
async fn test_run_function_response_resumes_workflow_sub_agent() {
    let cases: Vec<(&str, RootOf2, &str, usize)> = vec![
        (
            "parallel",
            |c, o| parallel("root", vec![c, o]),
            "root.caller",
            1,
        ),
        (
            "sequential, caller runs second",
            |c, o| sequential("root", vec![o, c]),
            "",
            1,
        ),
        (
            "sequential, caller runs first",
            |c, o| sequential("root", vec![c, o]),
            "",
            1,
        ),
    ];
    for (name, new_root, want_branch, want_other_calls) in cases {
        let approve = FunctionTool::new(
            FunctionToolConfig {
                is_long_running: true,
                ..FunctionToolConfig::new("approve", "approve")
            },
            |_ctx: Context, _a: NoArgs| async { Ok::<Option<Map<String, Value>>, ToolError>(None) },
        )
        .unwrap();
        let caller_model = MockModel::from_contents(vec![
            Content::from_function_call(
                FunctionCall {
                    id: "call-1".into(),
                    name: "approve".into(),
                    args: Map::new(),
                    ..Default::default()
                },
                "model",
            ),
            model_text("approved"),
        ]);
        let other_model = MockModel::from_contents(vec![model_text("other answer")]);
        let caller = llm_agent::new(LlmAgentConfig {
            name: "caller".into(),
            model: Some(caller_model.clone() as Arc<dyn Llm>),
            tools: vec![approve],
            ..Default::default()
        })
        .unwrap();
        let other = llm_agent::new(LlmAgentConfig {
            name: "other".into(),
            model: Some(other_model.clone() as Arc<dyn Llm>),
            ..Default::default()
        })
        .unwrap();
        let root = new_root(caller, other);
        let svc: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
        let r = runner_over(root, "app", svc.clone());
        let created = svc
            .create(CreateRequest {
                app_name: "app".into(),
                user_id: "u".into(),
                session_id: None,
                state: None,
            })
            .await
            .unwrap();
        let session_id = created.id().to_string();

        drain(run(&r, &session_id, user_text("go"))).await;

        let response = Content::from_function_response(
            FunctionResponse {
                id: "call-1".into(),
                name: "approve".into(),
                response: json!({"ok": true}).as_object().cloned().unwrap(),
                ..Default::default()
            },
            "user",
        );
        let got = drain(run(&r, &session_id, response)).await;

        assert_eq!(
            got.len(),
            1,
            "{name}: resume yielded {} events, want 1 from the caller",
            got.len()
        );
        assert_eq!(
            (got[0].author.as_str(), got[0].branch.as_str()),
            ("caller", want_branch),
            "{name}: resume event author/branch"
        );
        assert_eq!(
            got[0].response.content.as_ref().unwrap().parts[0].text,
            "approved",
            "{name}: resume event text"
        );
        let reqs = caller_model.requests();
        assert_eq!(reqs.len(), 2, "{name}: caller model calls");
        assert!(
            has_function_response(&reqs[1].contents, "call-1"),
            "{name}: caller's second model request has no function response for call-1"
        );
        assert_eq!(
            other_model.requests().len(),
            want_other_calls,
            "{name}: other agent model calls (it must not re-run on resume)"
        );
    }
}

type RootOf1 = fn(Arc<dyn Agent>) -> Arc<dyn Agent>;

#[tokio::test]
async fn test_run_function_response_inside_graph_workflow_resumes_workflow() {
    let cases: Vec<(&str, RootOf1)> = vec![
        ("graph workflow root", |wf| wf),
        ("graph workflow under sequential", |wf| {
            sequential("root", vec![wf])
        }),
        ("graph workflow under parallel", |wf| {
            parallel("root", vec![wf])
        }),
    ];
    for (name, new_root) in cases {
        let wf = two_node_workflow(
            "wf",
            join_resume_agent("b", "B", true),
            join_resume_agent("d", "D", false),
        );
        let r = runner_over(
            new_root(wf),
            "repro",
            Arc::new(InMemorySessionService::new()),
        );
        drain(run(&r, "s", user_text("start"))).await;
        let resumed = drain(run(
            &r,
            "s",
            resume_content("b", WORKFLOW_INPUT_FUNCTION_CALL_NAME, json!("approved")),
        ))
        .await;
        assert!(
            resumed
                .iter()
                .any(|ev| ev.author == "d" && ev.response.content.is_some()),
            "{name}: resume did not reach downstream node d"
        );
    }
}

type RootOfMany = fn(Vec<Arc<dyn Agent>>) -> Arc<dyn Agent>;

#[tokio::test]
async fn test_run_function_response_skips_siblings_of_graph_workflow() {
    let cases: Vec<(&str, RootOfMany, bool)> = vec![
        (
            "sequential, sibling ahead",
            |s| sequential("root", s),
            false,
        ),
        (
            "sequential, sub-agent after the workflow runs",
            |s| sequential("root", s),
            true,
        ),
        (
            "sequential inside sequential",
            |s| sequential("root", vec![sequential("inner", s)]),
            false,
        ),
        ("parallel", |s| parallel("root", s), false),
        (
            "parallel, sibling after the workflow",
            |s| parallel("root", vec![s[1].clone(), s[0].clone()]),
            false,
        ),
        ("loop", |s| looped("root", 1, s), false),
    ];
    for (name, new_root, with_after) in cases {
        let other_model = MockModel::from_contents(vec![model_text("other answer")]);
        let other = llm_agent::new(LlmAgentConfig {
            name: "other".into(),
            model: Some(other_model as Arc<dyn Llm>),
            ..Default::default()
        })
        .unwrap();
        let wf = two_node_workflow(
            "wf",
            join_resume_agent("b", "B", true),
            join_resume_agent("d", "D", false),
        );
        let mut subs = vec![other, wf];
        if with_after {
            subs.push(join_resume_agent("after", "A", false));
        }
        let r = runner_over(
            new_root(subs),
            "repro",
            Arc::new(InMemorySessionService::new()),
        );
        drain(run(&r, "s", user_text("start"))).await;
        let mut s = run(
            &r,
            "s",
            resume_content("b", WORKFLOW_INPUT_FUNCTION_CALL_NAME, json!("approved")),
        );
        let mut authors = std::collections::HashSet::new();
        while let Some(item) = s.next().await {
            let ev = item.unwrap_or_else(|e| panic!("{name}: resume error: {e}"));
            authors.insert(ev.author);
        }
        assert!(
            authors.contains("d"),
            "{name}: resume did not reach downstream node d"
        );
        assert!(
            !authors.contains("other"),
            "{name}: resume re-ran the sibling other"
        );
        if with_after {
            assert!(
                authors.contains("after"),
                "{name}: resume did not run the sub-agent after the workflow"
            );
        }
    }
}

type LoopRoot = fn(Arc<dyn Agent>, Arc<dyn Agent>) -> Arc<dyn Agent>;

#[tokio::test]
async fn test_run_function_response_narrows_only_first_loop_pass() {
    let cases: Vec<(&str, LoopRoot)> = vec![
        ("loop", |o, wf| looped("root", 2, vec![o, wf])),
        ("sequential inside loop", |o, wf| {
            looped("root", 2, vec![sequential("inner", vec![o, wf])])
        }),
        ("parallel inside loop", |o, wf| {
            looped("root", 2, vec![parallel("inner", vec![o, wf])])
        }),
        ("loop inside loop", |o, wf| {
            looped("root", 2, vec![looped("inner", 1, vec![o, wf])])
        }),
    ];
    for (name, new_root) in cases {
        let runs = Arc::new(AtomicI32::new(0));
        let other = custom_agent("other", runs.clone(), None);
        let root = new_root(other, paused_workflow("wf", "b", "d"));
        let (authors, err) = run_resume_turn(
            root,
            resume_content("b", WORKFLOW_INPUT_FUNCTION_CALL_NAME, json!("approved")),
            || runs.store(0, SeqCst),
        )
        .await;
        assert!(err.is_none(), "{name}: resume error: {err:?}");
        assert!(
            authors.get("d").copied().unwrap_or(0) > 0,
            "{name}: resume did not reach downstream node d, authors = {authors:?}"
        );
        assert_eq!(
            runs.load(SeqCst),
            1,
            "{name}: other runs on the resume turn (second pass only)"
        );
    }
}

#[tokio::test]
async fn test_run_function_response_escalator_in_loop_terminates() {
    let runs = Arc::new(AtomicI32::new(0));
    let esc = custom_agent(
        "esc",
        runs,
        Some(Arc::new(|ctx: &InvocationContext, ev: &mut Event| {
            if ctx
                .session()
                .events()
                .iter()
                .any(|e| e.author == "b" || e.author == "d")
            {
                ev.actions.escalate = true;
            }
        })),
    );
    let root = looped(
        "root",
        0,
        vec![sequential(
            "inner",
            vec![esc, paused_workflow("wf", "b", "d")],
        )],
    );
    let (authors, err) = run_resume_turn(
        root,
        resume_content("b", WORKFLOW_INPUT_FUNCTION_CALL_NAME, json!("approved")),
        || {},
    )
    .await;
    assert!(
        err.is_none(),
        "resume did not terminate: authors = {authors:?}, err = {err:?}"
    );
    assert!(
        authors.get("d").copied().unwrap_or(0) > 0,
        "resume did not reach downstream node d, authors = {authors:?}"
    );
}

type SelectRoot =
    fn(Arc<dyn Agent>, Arc<dyn Agent>, Arc<dyn Agent>, Arc<dyn Agent>) -> Arc<dyn Agent>;

/// (name, root builder, response parts, authors that must run, sibling runs wanted).
type SelectCase = (&'static str, SelectRoot, Vec<Part>, Vec<&'static str>, i32);

#[tokio::test]
async fn test_run_function_responses_select_every_caller() {
    const EXTERNAL_CALL_ID: &str = "external-call";
    let first_part = |c: Content| -> Part { c.parts.into_iter().next().unwrap() };
    let ask_a = first_part(resume_content(
        "a",
        WORKFLOW_INPUT_FUNCTION_CALL_NAME,
        json!("approved"),
    ));
    let ask_b = first_part(resume_content(
        "b",
        WORKFLOW_INPUT_FUNCTION_CALL_NAME,
        json!("approved"),
    ));
    let answer_external = first_part(resume_content(
        EXTERNAL_CALL_ID,
        "external",
        json!("approved"),
    ));
    let unknown = first_part(resume_content(
        "unknown-call",
        "external",
        json!("approved"),
    ));

    let cases: Vec<SelectCase> = vec![
        (
            "parallel resumes both workflows, not the sibling",
            |a, b, o, _| parallel("root", vec![a, o, b]),
            vec![ask_a.clone(), ask_b.clone()],
            vec!["c", "d"],
            0,
        ),
        (
            "parallel, parts in reverse order",
            |a, b, o, _| parallel("root", vec![a, o, b]),
            vec![ask_b.clone(), ask_a.clone()],
            vec!["c", "d"],
            0,
        ),
        (
            "sequential starts from the lowest index",
            |a, b, o, _| sequential("root", vec![o, a, b]),
            vec![ask_b.clone(), ask_a.clone()],
            vec!["c", "d"],
            0,
        ),
        (
            "loop starts from the lowest index",
            |a, b, o, _| looped("root", 1, vec![o, a, b]),
            vec![ask_b.clone(), ask_a.clone()],
            vec!["c", "d"],
            0,
        ),
        (
            "parallel of sequences narrows inside each",
            |a, b, o, _| parallel("root", vec![sequential("seqA", vec![o, a]), b]),
            vec![ask_a.clone(), ask_b.clone()],
            vec!["c", "d"],
            0,
        ),
        (
            "parallel also runs a sub-agent outside a graph that gets a response",
            |a, _, o, c| parallel("root", vec![a, o, c]),
            vec![ask_a.clone(), answer_external.clone()],
            vec!["c", "caller"],
            0,
        ),
        (
            "no narrowing without a caller in a graph",
            |_, _, o, c| parallel("root", vec![o, c]),
            vec![unknown.clone(), answer_external.clone()],
            vec!["caller"],
            1,
        ),
    ];
    for (name, new_root, parts, want_ran, want_other) in cases {
        let other_runs = Arc::new(AtomicI32::new(0));
        let other = custom_agent("other", other_runs.clone(), None);
        let caller = custom_agent(
            "caller",
            Arc::new(AtomicI32::new(0)),
            Some(Arc::new(|_ctx: &InvocationContext, ev: &mut Event| {
                ev.response.content = Some(Content::from_function_call(
                    FunctionCall {
                        id: EXTERNAL_CALL_ID.into(),
                        name: "external".into(),
                        ..Default::default()
                    },
                    "model",
                ));
            })),
        );
        let root = new_root(
            paused_workflow("wfA", "a", "c"),
            paused_workflow("wfB", "b", "d"),
            other,
            caller,
        );
        let msg = Content {
            role: "user".into(),
            parts,
        };
        let (authors, err) = run_resume_turn(root, msg, || other_runs.store(0, SeqCst)).await;
        assert!(
            err.is_none(),
            "{name}: resume error: {err:?}, authors = {authors:?}"
        );
        for want in want_ran {
            assert!(
                authors.get(want).copied().unwrap_or(0) > 0,
                "{name}: {want} did not run on the resume turn, authors = {authors:?}"
            );
        }
        assert_eq!(
            other_runs.load(SeqCst),
            want_other,
            "{name}: sibling other runs on the resume turn"
        );
    }
}

#[tokio::test]
async fn test_run_function_response_reaches_llm_agent_after_workflow() {
    for silent in [true, false] {
        let name = if silent {
            "last node emits no content"
        } else {
            "last node emits content"
        };
        let last = custom_agent(
            "last",
            Arc::new(AtomicI32::new(0)),
            Some(Arc::new(move |_ctx: &InvocationContext, ev: &mut Event| {
                if silent {
                    ev.response.content = None;
                }
            })),
        );
        let wf = two_node_workflow("wf", join_resume_agent("b", "B", true), last);
        let after_model = MockModel::from_contents(vec![model_text("first"), model_text("second")]);
        let after = llm_agent::new(LlmAgentConfig {
            name: "after".into(),
            model: Some(after_model.clone() as Arc<dyn Llm>),
            ..Default::default()
        })
        .unwrap();
        let (authors, err) = run_resume_turn(
            sequential("root", vec![wf, after]),
            resume_content("b", WORKFLOW_INPUT_FUNCTION_CALL_NAME, json!("approved")),
            || {},
        )
        .await;
        assert!(
            err.is_none(),
            "{name}: resume error: {err:?}, authors = {authors:?}"
        );
        assert!(
            authors.get("after").copied().unwrap_or(0) > 0,
            "{name}: after did not run on the resume turn, authors = {authors:?}"
        );
        let reqs = after_model.requests();
        let texts = content_texts(&reqs.last().unwrap().contents);
        assert!(
            texts.iter().any(|s| s.contains("approved")),
            "{name}: after's request does not carry the user's answer, texts = {texts:?}"
        );
    }
}
