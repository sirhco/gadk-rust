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

//! Port of Go agent/llmagent/llmagent_task_outputkey_test.go.

use std::sync::Arc;

use futures::StreamExt;
use gadk_agent::{
    Agent, AgentConfig, BeforeAgentCallback, Context, Mode, RunConfig, StreamingMode,
};
use gadk_genai::{Content, ROLE_MODEL, ROLE_USER};
use gadk_platform::Platform;
use gadk_session::{CreateRequest, GetRequest, InMemorySessionService, SessionService};
use serde_json::{Map, Value, json};

use super::agent_llmagent_llm_agent_wrapper::{
    ScriptedLlm, fc_resp, make_llm_agent, schema, text_resp,
};
use crate::agents::sequential_agent::{self, SequentialAgentConfig};
use crate::agents::workflow_agent::{self, WorkflowAgentConfig};
use crate::runner::{PluginConfig, RunOptions, Runner, RunnerConfig};
use crate::workflow::{
    AgentNode, Node, NodeConfig, RunNodeOptions, START, chain, dynamic_node, run_node,
};

/// Runs `agent` for one "go" turn over a fresh in-memory session ("app"/"u"/"s") and returns the
/// session state after the run.
async fn run_and_collect_state(agent: Arc<dyn Agent>, cfg: RunConfig) -> Map<String, Value> {
    let svc: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    svc.create(CreateRequest {
        app_name: "app".into(),
        user_id: "u".into(),
        session_id: Some("s".into()),
        state: None,
    })
    .await
    .unwrap_or_else(|e| panic!("session.Create: {e}"));
    let r = Runner::new(RunnerConfig {
        app_name: "app".into(),
        agent,
        session_service: svc.clone(),
        artifact_service: None,
        memory_service: None,
        plugin_config: PluginConfig::default(),
        auto_create_session: false,
        platform: Platform::default(),
        compaction: None,
    })
    .unwrap_or_else(|e| panic!("runner.New: {e}"));
    let mut stream = r.run(
        "u",
        "s",
        Some(Content::from_text("go", ROLE_USER)),
        cfg,
        RunOptions::default(),
    );
    while let Some(ev) = stream.next().await {
        ev.unwrap_or_else(|e| panic!("runner.Run: {e}"));
    }
    let sess = svc
        .get(GetRequest {
            app_name: "app".into(),
            user_id: "u".into(),
            session_id: "s".into(),
            ..Default::default()
        })
        .await
        .unwrap_or_else(|e| panic!("session.Get: {e}"));
    sess.state().all().into_iter().collect()
}

/// Port of Go runCoordinatorWithTask: runs a chat coordinator whose only turn delegates to
/// `task_agent`, and returns the session state after the run.
async fn run_coordinator_with_task(task_agent: Arc<dyn Agent>) -> Map<String, Value> {
    let coord_llm = ScriptedLlm::new(vec![
        fc_resp("fc-task", task_agent.name(), json!({"request": "do it"})),
        text_resp("ok"),
    ]);
    let coord = make_llm_agent("coord", Mode::Chat, |c| {
        c.description = String::new();
        c.model = Some(coord_llm);
        c.sub_agents = vec![task_agent];
    });
    run_and_collect_state(
        coord,
        RunConfig {
            streaming_mode: StreamingMode::Sse,
            ..Default::default()
        },
    )
    .await
}

/// A task agent delivers its result through finish_task. A text turn before that (a
/// clarification question, say) is not the result and must not be written to OutputKey.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "8a bug: maybe_save_output_to_state writes a task agent's text turns to output_key (Go #1977 skips task mode)"]
async fn test_task_agent_text_turn_does_not_write_output_key() {
    let task_llm = ScriptedLlm::new(vec![text_resp("Which language do you want?")]);
    let task_agent = make_llm_agent("doer", Mode::Task, |c| {
        c.output_key = "task_out".into();
        c.model = Some(task_llm);
    });

    let state = run_coordinator_with_task(task_agent).await;

    assert!(
        !state.contains_key("task_out"),
        "state[\"task_out\"] = {:?} after a task agent's text turn, want it unset",
        state.get("task_out")
    );
}

/// When the task completes, OutputKey holds the finish_task output, the same value as the
/// terminal event's Output: the value under the wrapper key for a wrapped primitive, otherwise
/// the args map.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "8a bug: run_task does not write the accepted finish_task output to output_key (Go #1977)"]
async fn test_task_agent_finish_task_writes_output_key() {
    let cases = [
        (
            "object output",
            None,
            json!({"result": "summarized"}),
            json!({"result": "summarized"}),
        ),
        (
            "wrapped primitive",
            Some(schema(json!({"type": "STRING"}))),
            json!({"result": "summarized"}),
            json!("summarized"),
        ),
    ];
    for (name, output_schema, args, want) in cases {
        let task_llm = ScriptedLlm::new(vec![fc_resp("finish-1", "finish_task", args)]);
        let task_agent = make_llm_agent("doer", Mode::Task, |c| {
            c.output_key = "task_out".into();
            c.model = Some(task_llm);
            c.output_schema = output_schema;
        });

        let state = run_coordinator_with_task(task_agent).await;

        let got = state.get("task_out").unwrap_or_else(|| {
            panic!("{name}: state[\"task_out\"] is unset after finish_task, want the finish_task output")
        });
        assert_eq!(got, &want, "{name}: state[\"task_out\"] mismatch");
    }
}

/// Without an OutputKey, finishing a task writes nothing to state.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_task_agent_finish_task_without_output_key_writes_no_state() {
    let task_llm = ScriptedLlm::new(vec![fc_resp(
        "finish-1",
        "finish_task",
        json!({"result": "summarized"}),
    )]);
    let task_agent = make_llm_agent("doer", Mode::Task, |c| c.model = Some(task_llm));

    let state = run_coordinator_with_task(task_agent).await;

    assert!(state.is_empty(), "session state = {state:?}, want it empty");
}

/// Content returned by a before-agent callback is not a task result either, so it must not
/// reach OutputKey on a task agent.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "8a bug: maybe_save_output_to_state writes a task agent's text turns to output_key (Go #1977 skips task mode)"]
async fn test_task_agent_before_agent_callback_content_does_not_write_output_key() {
    let before: BeforeAgentCallback = Arc::new(|_ctx: Context| {
        Box::pin(async { Ok(Some(Content::from_text("cached answer", ROLE_MODEL))) })
    });
    let task_agent = make_llm_agent("doer", Mode::Task, |c| {
        c.output_key = "task_out".into();
        c.before_agent_callbacks = vec![before];
    });

    let state = run_coordinator_with_task(task_agent).await;

    assert!(
        !state.contains_key("task_out"),
        "state[\"task_out\"] = {:?} after a before-agent callback short-circuit on a task agent, want it unset",
        state.get("task_out")
    );
}

/// A finish_task call that fails validation is not a result. OutputKey must stay unset until a
/// call is accepted, so the write belongs on the success response, not on the call.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "8a bug: maybe_save_output_to_state writes a task agent's text turns to output_key (Go #1977 skips task mode)"]
async fn test_task_agent_rejected_finish_task_does_not_write_output_key() {
    let output_schema = schema(json!({
        "type": "OBJECT",
        "properties": {"answer": {"type": "STRING"}},
        "required": ["answer"]
    }));
    let task_llm = ScriptedLlm::new(vec![fc_resp(
        "finish-bad",
        "finish_task",
        json!({"wrong_field": "oops"}),
    )]);
    let task_agent = make_llm_agent("doer", Mode::Task, |c| {
        c.output_key = "task_out".into();
        c.output_schema = Some(output_schema);
        c.model = Some(task_llm);
    });

    let state = run_coordinator_with_task(task_agent).await;

    assert!(
        !state.contains_key("task_out"),
        "state[\"task_out\"] = {:?} after only a rejected finish_task, want it unset",
        state.get("task_out")
    );
}

/// A SequentialAgent calls Run on its children, not RunNode, so a task agent under one does not
/// go through runTask and nothing writes the finish_task output. OutputKey stays unset, and the
/// text turn after finish_task does not fill it either. adk-python behaves the same way. This
/// pins the limit of the OutputKey behavior for task agents run through Run.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "8a bug: maybe_save_output_to_state writes a task agent's text turns to output_key (Go #1977 skips task mode)"]
async fn test_sequential_parent_task_child_leaves_output_key_unset() {
    let task_llm = ScriptedLlm::new(vec![
        fc_resp("finish-1", "finish_task", json!({"result": "summarized"})),
        text_resp("all done"),
    ]);
    let task_agent = make_llm_agent("doer", Mode::Task, |c| {
        c.output_key = "task_out".into();
        c.model = Some(task_llm);
    });
    let seq = sequential_agent::new(SequentialAgentConfig {
        agent: AgentConfig {
            name: "seq".into(),
            sub_agents: vec![task_agent],
            ..Default::default()
        },
    })
    .unwrap_or_else(|e| panic!("sequentialagent.New: {e}"));

    let state = run_and_collect_state(seq, RunConfig::default()).await;

    assert!(
        !state.contains_key("task_out"),
        "state[\"task_out\"] = {:?} under a sequential parent, want it unset",
        state.get("task_out")
    );
}

/// A task agent cannot sit on the edges of a workflowagent graph directly; it is dispatched with
/// workflow.RunNode from inside another node. That enters the task agent through its RunNode
/// method, so runTask runs and OutputKey holds the finish_task output, unlike under a
/// SequentialAgent.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "8a bug: run_task does not write the accepted finish_task output to output_key (Go #1977)"]
async fn test_workflow_agent_dispatched_task_writes_output_key() {
    let task_llm = ScriptedLlm::new(vec![fc_resp(
        "finish-1",
        "finish_task",
        json!({"result": "summarized"}),
    )]);
    let task_agent = make_llm_agent("doer", Mode::Task, |c| {
        c.output_key = "task_out".into();
        c.model = Some(task_llm);
    });
    let task_node: Arc<dyn Node> = AgentNode::new(task_agent, NodeConfig::default())
        .unwrap_or_else(|e| panic!("workflow.NewAgentNode: {e}"));
    let router = dynamic_node::<String, String, _, _>(
        "router",
        move |ctx: Context, input: String, _emit| {
            let task_node = task_node.clone();
            async move {
                run_node(&ctx, task_node, json!(input), RunNodeOptions::default()).await?;
                Ok("routed".to_string())
            }
        },
        NodeConfig::default(),
    );
    let wf = workflow_agent::new(WorkflowAgentConfig {
        name: "wf".into(),
        edges: chain(&[START.clone(), router]),
        ..Default::default()
    })
    .unwrap_or_else(|e| panic!("workflowagent.New: {e}"));

    let state = run_and_collect_state(wf, RunConfig::default()).await;

    let got = state.get("task_out").unwrap_or_else(|| {
        panic!("state[\"task_out\"] is unset after a dispatched task agent's finish_task")
    });
    assert_eq!(
        got,
        &json!({"result": "summarized"}),
        "state[\"task_out\"] mismatch"
    );
}
