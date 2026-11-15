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

//! Goroutine-attribution ports (7b-3b) of ADK-Go `plugin/agentanalytics/trace_manager_test.go`:
//! group A: the goroutine-identity unit tests (`parseGoroutineTrace`, `currentGoroutine`, the
//! creators and attempt sites), adapted to `gadk_agent::fiber`.
//! Shared helpers: [`attr_support`](super::attr_support).
//!
//! Go reads the goroutine's id, creator and frames out of `runtime.Stack`; gadk reads the fiber
//! the engine set ([`fiber::current`]), whose origin names one of the `gadk_agent::fiber` creator
//! constants, and the attempt slot the engine marked ([`fiber::current_attempt`]). Each port
//! asserts Go's facts about the substitute: ids, origins and attempt sites; Go's stack-text cases
//! (header fields, elided frames, `iter.Pull` goroutines) have no gadk counterpart.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::sync::Arc;

use futures::{FutureExt, StreamExt};
use gadk_agent::fiber::{
    self, AttemptSite, ERRGROUP_CREATOR, Fiber, Origin, PARALLEL_WORKER_CREATOR, SCHEDULER_CREATOR,
};
use gadk_agent::{Agent, AgentConfig, AgentError, BeforeAgentCallback, Context, InvocationContext};
use gadk_engine::llm::LlmAgentConfig;
use gadk_engine::workflow::{JoinNode, Node, NodeConfig, ParallelWorker, function_node};
use parking_lot::Mutex;
use serde_json::Value;

use super::attr_support::{agent_node, edge, go, on_test_goroutine, start};
use super::runner_support::{
    app_runner, final_text, llm_agent, new_event, run_until_error, scripted_llm, text_content,
};

/// The bit of a fallback fiber id (a tokio task id, `gadk_agent::fiber::current`).
const FALLBACK_BIT: u64 = 1 << 63;

/// A before-agent callback that runs `f` with the agent's name (Go's `record` callbacks).
fn recording(f: impl Fn(&str) + Send + Sync + 'static) -> BeforeAgentCallback {
    Arc::new(move |ctx: Context| {
        f(&ctx.agent_name());
        async { Ok(None) }.boxed()
    })
}

/// A custom agent named `name` that runs `subs` one after another, then answers (Go
/// `newAgent(name, subAgents...)` of `TestAttemptSitesMatchADK`), with `callbacks`.
fn answering_agent(
    name: &str,
    subs: Vec<Arc<dyn Agent>>,
    callbacks: Vec<BeforeAgentCallback>,
) -> Arc<dyn Agent> {
    let owned = name.to_owned();
    let run_subs = subs.clone();
    gadk_agent::new_agent(AgentConfig {
        name: name.into(),
        sub_agents: subs,
        before_agent_callbacks: callbacks,
        run: Some(Arc::new(move |ctx: InvocationContext| {
            let (name, subs) = (owned.clone(), run_subs.clone());
            futures::stream::once(async move {
                for sub in subs {
                    let mut events = sub.run(ctx.clone());
                    while let Some(item) = events.next().await {
                        if let Err(e) = item {
                            return vec![Err(e)];
                        }
                    }
                }
                let mut ev = new_event(&ctx);
                ev.author = name.clone();
                ev.branch = ctx.branch();
                ev.response.content = Some(text_content("model", &format!("{name} answer")));
                vec![Ok(ev)]
            })
            .flat_map(futures::stream::iter)
            .boxed()
        })),
        ..Default::default()
    })
    .expect("custom agent")
}

/// Go `first_run_error(seq)`: the first error of `a`'s run in `ctx`, if any.
async fn first_run_error(a: &Arc<dyn Agent>, ctx: InvocationContext) -> Result<(), AgentError> {
    let mut events = a.run(ctx);
    while let Some(item) = events.next().await {
        item?;
    }
    Ok(())
}

/// The run context of `a` alone (no runner).
fn run_ctx(a: &Arc<dyn Agent>) -> InvocationContext {
    InvocationContext::builder()
        .agent(a.clone())
        .build_invocation()
}

// parity: adapted: gadk has no goroutine trace to parse; Go's header and "created by" cases are the fiber bookkeeping they stand for: new ids, creator + parent origins, zero origins for roots and fallbacks, fallback ids for unscoped tasks, 0 outside any task (no stack text, no truncation)
#[tokio::test]
async fn test_parse_goroutine_trace() {
    // "main goroutine": a root fiber has an id and no origin.
    let main = Fiber::root();
    assert!(
        main.id != 0 && main.id < FALLBACK_BIT,
        "root id {}",
        main.id
    );
    assert_eq!(main.origin, Origin::default(), "root origin");

    // "created by …startNode in goroutine 7" / "created by …errgroup.(*Group).Go in goroutine 9".
    let parent_id = main.id;
    let (sched, group) = main
        .scope(async {
            (
                fiber::spawn_child(SCHEDULER_CREATOR),
                fiber::spawn_child(ERRGROUP_CREATOR),
            )
        })
        .await;
    for (child, creator) in [(&sched, SCHEDULER_CREATOR), (&group, ERRGROUP_CREATOR)] {
        assert!(
            child.id > parent_id && child.id < FALLBACK_BIT,
            "id {} of a fiber started by {creator}, want a new id after {parent_id}",
            child.id
        );
        assert_eq!(
            child.origin,
            Origin {
                creator,
                parent: parent_id,
            },
            "origin of a fiber started by {creator}"
        );
    }
    assert!(sched.id < group.id, "fiber ids strictly increase");

    // "traceback ancestors": the origin names only the direct creator and parent.
    let sched_id = sched.id;
    let grandchild = sched
        .scope(async { fiber::spawn_child(PARALLEL_WORKER_CREATOR) })
        .await;
    assert_eq!(
        grandchild.origin,
        Origin {
            creator: PARALLEL_WORKER_CREATOR,
            parent: sched_id,
        },
        "origin of a grandchild"
    );

    // "created by without goroutine" / malformed: a fiber started where no fiber is known has an
    // unknown parent (0), which never counts as started by anything.
    let orphan = fiber::spawn_child(SCHEDULER_CREATOR);
    assert_eq!(orphan.origin.parent, 0, "parent outside any fiber or task");
    assert!(!orphan.origin.started_by(SCHEDULER_CREATOR, 0));

    // "no header" / "empty": outside any fiber or task the id is unknown (0), in a task without
    // a fiber it is a fallback id above every fiber id, with a zero origin.
    assert_eq!(
        fiber::current().id,
        0,
        "id in a test body outside any fiber"
    );
    let fallback = go(async { fiber::current() }).await.unwrap();
    assert!(fallback.id > FALLBACK_BIT, "fallback id {}", fallback.id);
    assert_eq!(fallback.origin, Origin::default(), "fallback origin");
}

// parity: adapted: Go's `go` statement names the test function and its goroutine; gadk names the spawn site given to fiber::spawn_child, and a plain tokio::spawn gets a fallback fiber
#[tokio::test]
async fn test_current_goroutine() {
    const TEST_CREATOR: &str = "attr_fiber::test_current_goroutine";
    on_test_goroutine(async {
        let parent = fiber::current();
        assert_ne!(parent.id, 0, "current() returned no ID");
        let child = fiber::spawn_child(TEST_CREATOR);
        let got = tokio::spawn(child.scope(async { fiber::current() }))
            .await
            .unwrap();
        assert!(
            got.id != 0 && got.id != parent.id,
            "current() ID = {}, want a new fiber's (not {})",
            got.id,
            parent.id
        );
        assert_eq!(
            got.origin,
            Origin {
                creator: TEST_CREATOR,
                parent: parent.id,
            },
            "current() origin, want {TEST_CREATOR} in fiber {}",
            parent.id
        );

        // A task started without a fiber runs on a fallback fiber of its own.
        let plain = go(async { fiber::current() }).await.unwrap();
        assert!(
            plain.id != 0 && plain.id != parent.id && plain.id != got.id,
            "fallback id {}",
            plain.id
        );
        assert_eq!(plain.origin, Origin::default(), "fallback origin");
        // The test fiber is current again after the awaits.
        assert_eq!(fiber::current().id, parent.id);
    })
    .await;
}

// parity: adapted: Go compares runtime.Stack creators with schedulerCreator / errgroupCreator; gadk compares fiber origins with the gadk_agent::fiber constants, plus a ParallelWorker item (PARALLEL_WORKER_CREATOR), whose spawning fiber runs no agent to record its id
#[tokio::test]
async fn test_goroutine_creators_match_adk() {
    type Starts = Arc<Mutex<BTreeMap<String, (u64, Origin)>>>;
    let starts = Starts::default();
    let record = |starts: &Starts| {
        let starts = starts.clone();
        recording(move |name| {
            let f = fiber::current();
            starts.lock().insert(name.to_owned(), (f.id, f.origin));
        })
    };
    let leaf = answering_agent("leaf", vec![], vec![record(&starts)]);
    let par = gadk_engine::parallel_agent::new(gadk_engine::parallel_agent::ParallelAgentConfig {
        agent: AgentConfig {
            name: "par".into(),
            sub_agents: vec![leaf],
            before_agent_callbacks: vec![record(&starts)],
            ..Default::default()
        },
    })
    .unwrap();
    let node = agent_node(par);
    let items: Arc<dyn Node> = function_node(
        "items",
        |_ctx: Context, _in: Value| async { Ok::<_, AgentError>(vec!["a"]) },
        NodeConfig::default(),
    );
    let item_agent = answering_agent("item_agent", vec![], vec![record(&starts)]);
    let per_item: Arc<dyn Node> =
        ParallelWorker::new("per_item", agent_node(item_agent), 0, NodeConfig::default()).unwrap();
    let done: Arc<dyn Node> = JoinNode::new("done");
    let root = gadk_engine::workflow_agent::new(gadk_engine::workflow_agent::WorkflowAgentConfig {
        name: "root".into(),
        edges: vec![
            edge(&start(), &node),
            edge(&start(), &items),
            edge(&items, &per_item),
            edge(&node, &done),
            edge(&per_item, &done),
        ],
        before_agent_callbacks: vec![record(&starts)],
        ..Default::default()
    })
    .unwrap();
    let (_, errs) = run_until_error(&app_runner("app", root, vec![]), "go").await;
    assert!(errs.is_empty(), "runner.Run error: {errs:?}");

    let starts = starts.lock();
    let (root_id, _) = starts["root"];
    let (par_id, par_origin) = starts["par"];
    assert!(root_id != 0, "root ran on no fiber");
    assert_eq!(
        par_origin,
        Origin {
            creator: SCHEDULER_CREATOR,
            parent: root_id,
        },
        "origin of the agent node's fiber"
    );
    assert_eq!(
        starts["leaf"].1,
        Origin {
            creator: ERRGROUP_CREATOR,
            parent: par_id,
        },
        "origin of the parallel branch's fiber"
    );
    let item = starts["item_agent"].1;
    assert!(
        item.creator == PARALLEL_WORKER_CREATOR
            && item.parent != 0
            && item.parent != root_id
            && item.parent != par_id,
        "origin of the ParallelWorker item's fiber = {item:?}, want {PARALLEL_WORKER_CREATOR} in the worker node's fiber"
    );
}

// parity: adapted: Go's stack-shape cases are the fiber's attempt slot: marked by with_attempt, claimed by the first agent run on the fiber for its before-agent callbacks only, never by a nested run, consumed by the claim, cleared after the attempt; no fiber = no attempt
#[tokio::test]
async fn test_attempt_site_of_trace() {
    type Sites = Arc<Mutex<Vec<(String, Option<AttemptSite>)>>>;
    let sites = Sites::default();
    let record = {
        let sites = sites.clone();
        recording(move |name| {
            sites
                .lock()
                .push((name.to_owned(), fiber::current_attempt()));
        })
    };
    let take = |sites: &Sites| std::mem::take(&mut *sites.lock());
    let leaf = answering_agent("leaf", vec![], vec![record.clone()]);

    // "agent node" / "parallel worker item": the attempt's agent run claims the site.
    for site in [AttemptSite::Node, AttemptSite::Worker] {
        let ctx = run_ctx(&leaf);
        Fiber::root()
            .scope(fiber::with_attempt(site, first_run_error(&leaf, ctx)))
            .await
            .unwrap();
        assert_eq!(take(&sites), vec![("leaf".into(), Some(site))], "{site:?}");
    }

    // "function node" / "goroutine of the runner": nothing marked, no attempt.
    let ctx = run_ctx(&leaf);
    Fiber::root()
        .scope(first_run_error(&leaf, ctx))
        .await
        .unwrap();
    assert_eq!(
        take(&sites),
        vec![("leaf".into(), None)],
        "no attempt marked"
    );

    // "incomplete trace" / "empty trace": no fiber, so with_attempt marks nothing.
    let ctx = run_ctx(&leaf);
    fiber::with_attempt(AttemptSite::Node, first_run_error(&leaf, ctx))
        .await
        .unwrap();
    assert_eq!(take(&sites), vec![("leaf".into(), None)], "no fiber");

    // "agent run inside the run of a node's agent": only the outer run is the attempt.
    let outer = answering_agent("outer", vec![leaf.clone()], vec![record.clone()]);
    let ctx = run_ctx(&outer);
    Fiber::root()
        .scope(fiber::with_attempt(
            AttemptSite::Node,
            first_run_error(&outer, ctx),
        ))
        .await
        .unwrap();
    assert_eq!(
        take(&sites),
        vec![
            ("outer".into(), Some(AttemptSite::Node)),
            ("leaf".into(), None)
        ],
        "nested run"
    );

    // "agent run inside a callback of a node's agent": the nested run is none, and the node's
    // agent still sees its attempt after it.
    let nested_in_callback: BeforeAgentCallback = {
        let (sites, leaf) = (sites.clone(), leaf.clone());
        Arc::new(move |ctx: Context| {
            let (sites, leaf) = (sites.clone(), leaf.clone());
            async move {
                first_run_error(&leaf, ctx.as_invocation().clone()).await?;
                sites
                    .lock()
                    .push(("caller".into(), fiber::current_attempt()));
                Ok(None)
            }
            .boxed()
        })
    };
    let caller = answering_agent("caller", vec![], vec![nested_in_callback]);
    let ctx = run_ctx(&caller);
    Fiber::root()
        .scope(fiber::with_attempt(
            AttemptSite::Node,
            first_run_error(&caller, ctx),
        ))
        .await
        .unwrap();
    assert_eq!(
        take(&sites),
        vec![
            ("leaf".into(), None),
            ("caller".into(), Some(AttemptSite::Node))
        ],
        "run inside a callback"
    );

    // "not in a BeforeAgentCallback": the attempt is visible during the callbacks only; a second
    // run in the same attempt finds it consumed; after the attempt nothing is pending.
    let in_run = Arc::new(Mutex::new(Vec::new()));
    let body = {
        let in_run = in_run.clone();
        gadk_agent::new_agent(AgentConfig {
            name: "body".into(),
            before_agent_callbacks: vec![record.clone()],
            run: Some(Arc::new(move |ctx: InvocationContext| {
                in_run.lock().push(fiber::current_attempt());
                futures::stream::iter([Ok(new_event(&ctx))]).boxed()
            })),
            ..Default::default()
        })
        .unwrap()
    };
    let (c1, c2, c3) = (run_ctx(&body), run_ctx(&body), run_ctx(&body));
    let fiber = Fiber::root();
    fiber
        .clone()
        .scope(fiber::with_attempt(AttemptSite::Worker, async {
            first_run_error(&body, c1).await.unwrap();
            first_run_error(&body, c2).await.unwrap();
        }))
        .await;
    fiber.scope(first_run_error(&body, c3)).await.unwrap();
    assert_eq!(
        take(&sites),
        vec![
            ("body".into(), Some(AttemptSite::Worker)),
            ("body".into(), None),
            ("body".into(), None)
        ],
        "claimed once, consumed, cleared"
    );
    assert_eq!(
        *in_run.lock(),
        vec![None, None, None],
        "attempt in the run body"
    );
}

// parity: adapted: Go's iter.Pull2 child (pull_child) has no gadk counterpart and is left out of the run and of the want map; the other agents and their sites are Go's
#[tokio::test]
async fn test_attempt_sites_match_adk() {
    let sites: Arc<Mutex<BTreeMap<String, Vec<Option<AttemptSite>>>>> = Arc::default();
    let record = {
        let sites = sites.clone();
        recording(move |name| {
            sites
                .lock()
                .entry(name.to_owned())
                .or_default()
                .push(fiber::current_attempt());
        })
    };
    let new_agent =
        |name: &str, subs: Vec<Arc<dyn Agent>>| answering_agent(name, subs, vec![record.clone()]);
    let llm = llm_agent(LlmAgentConfig {
        name: "llm_node".into(),
        model: Some(scripted_llm(vec![final_text("llm answer")])),
        before_agent_callbacks: vec![record.clone()],
        ..Default::default()
    });
    let (fn_child, nested_child, item_child) = (
        new_agent("fn_child", vec![]),
        new_agent("nested_child", vec![]),
        new_agent("item_fn_child", vec![]),
    );
    let fn_node: Arc<dyn Node> = function_node(
        "fn",
        move |ctx: Context, _in: Value| {
            let (fn_child, nested_child) = (fn_child.clone(), nested_child.clone());
            async move {
                let ic = ctx.as_invocation().clone();
                first_run_error(&fn_child, ic.clone()).await?;
                let mut outer = nested_child.run(ic.clone());
                while let Some(item) = outer.next().await {
                    item?;
                    first_run_error(&nested_child, ic.clone()).await?;
                }
                Ok::<_, AgentError>("fn")
            }
        },
        NodeConfig::default(),
    );
    let items: Arc<dyn Node> = function_node(
        "items",
        |_ctx: Context, _in: Value| async { Ok::<_, AgentError>(vec!["a", "b"]) },
        NodeConfig::default(),
    );
    let per_item: Arc<dyn Node> = ParallelWorker::new(
        "per_item",
        agent_node(new_agent("worker_agent", vec![])),
        0,
        NodeConfig::default(),
    )
    .unwrap();
    let item_fn: Arc<dyn Node> = function_node(
        "item_fn",
        move |ctx: Context, _in: Value| {
            let item_child = item_child.clone();
            async move {
                first_run_error(&item_child, ctx.as_invocation().clone()).await?;
                Ok::<_, AgentError>("item_fn")
            }
        },
        NodeConfig::default(),
    );
    let per_item_fn: Arc<dyn Node> =
        ParallelWorker::new("per_item_fn", item_fn, 0, NodeConfig::default()).unwrap();
    let node_agent = agent_node(new_agent(
        "node_agent",
        vec![new_agent("sub_agent", vec![])],
    ));
    let llm_node = agent_node(llm);
    // A workflow may have only one terminal node with an output.
    let done: Arc<dyn Node> = JoinNode::new("done");
    let root = gadk_engine::workflow_agent::new(gadk_engine::workflow_agent::WorkflowAgentConfig {
        name: "root".into(),
        edges: vec![
            edge(&start(), &node_agent),
            edge(&start(), &llm_node),
            edge(&start(), &fn_node),
            edge(&start(), &items),
            edge(&items, &per_item),
            edge(&items, &per_item_fn),
            edge(&node_agent, &done),
            edge(&llm_node, &done),
            edge(&fn_node, &done),
            edge(&per_item, &done),
            edge(&per_item_fn, &done),
        ],
        before_agent_callbacks: vec![record.clone()],
        ..Default::default()
    })
    .unwrap();
    let (_, errs) = run_until_error(&app_runner("app", root, vec![]), "go").await;
    assert!(errs.is_empty(), "runner.Run error: {errs:?}");

    use AttemptSite::{Node as AttemptNode, Worker as AttemptWorker};
    let want: BTreeMap<String, Vec<Option<AttemptSite>>> = [
        ("root", vec![None]),
        ("node_agent", vec![Some(AttemptNode)]),
        ("sub_agent", vec![None]),
        ("llm_node", vec![Some(AttemptNode)]),
        ("fn_child", vec![None]),
        ("nested_child", vec![None, None]),
        (
            "worker_agent",
            vec![Some(AttemptWorker), Some(AttemptWorker)],
        ),
        ("item_fn_child", vec![None, None]),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v))
    .collect();
    assert_eq!(*sites.lock(), want, "attempt sites per agent");
}
