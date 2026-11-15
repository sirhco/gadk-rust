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

//! Part of the port of ADK-Go `plugin/agentanalytics/trace_manager_test.go`: the
//! 21 Go tests from `TestRunner_PriorPluginAnswersOnlyChild_EmitsInvocationCompletedOK` to
//! `TestCustomFanOutHandlesCanceledChild` in the Go file's order that run a gadk `Runner` (the
//! exact list is in the 7b-3a plan's contracts, Appendix A).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::sync::Arc;

use futures::StreamExt;
use gadk_agent::{AgentError, RunConfig};
use gadk_engine::llm::LlmAgentConfig;
use gadk_model::LlmResponse;
use serde_json::json;

use super::runner_support::*;
use crate::event_types::*;
use crate::parity_tests::plugin_agentanalytics_bigquery_agent_analytics_plugin::support::*;
use crate::row::Row;
use crate::trace::testkit::sequential;

/// A model answering "never sent" once.
fn never_sent() -> Arc<dyn gadk_model::Llm> {
    scripted_llm(vec![Ok(LlmResponse {
        content: Some(text_content("model", "never sent")),
        ..Default::default()
    })])
}

#[tokio::test]
async fn test_runner_prior_plugin_answers_only_child_emits_invocation_completed_ok() {
    for (name, drain) in [
        ("consumer stops at the answer of the child", false),
        ("consumer drains the run", true),
    ] {
        let tp = setup_test_plugin().await;
        let earlier = plugin_named(
            "earlier_before_agent",
            gadk_agent::PluginConfig {
                before_agent_callback: Some(Arc::new(|ctx: gadk_agent::Context| {
                    let answer = (ctx.agent_name() == "child")
                        .then(|| text_content("model", "answered by an earlier plugin"));
                    Box::pin(async move { Ok(answer) })
                })),
                ..Default::default()
            },
        );
        let child = llm_agent(LlmAgentConfig {
            name: "child".into(),
            model: Some(never_sent()),
            ..Default::default()
        });
        let seq = sequential("sequence", vec![child]);
        let r = new_test_runner(seq, vec![earlier, tp.plugin.clone()]);
        let mut got_answer = false;
        let (_, errs) = run_until(&r, "u1", "s1", "hi", RunConfig::default(), |item| {
            let answer =
                matches!(item, Ok(ev) if ev.author == "child" && ev.response.content.is_some());
            got_answer |= answer;
            answer && !drain
        })
        .await;
        assert!(errs.is_empty(), "{name}: r.Run error: {errs:?}");
        assert!(
            got_answer,
            "{name}: r.Run ended without the answer of the child"
        );

        let rows = RowCursor::default()
            .collect(&tp, until_invocation_end)
            .await;
        assert_eq!(
            rows_of_type(&rows, EVENT_AGENT_STARTING).len(),
            1,
            "{name}: AGENT_STARTING rows, want 1 (the sequence): the earlier plugin answers for the child first"
        );
        require_invocation_completed_ok(&rows);
    }
}

#[tokio::test]
async fn test_runner_recovered_model_error_with_skipped_after_model_emits_invocation_completed_ok()
{
    let tp = setup_test_plugin().await;
    let earlier = plugin_named(
        "earlier_after_model",
        gadk_agent::PluginConfig {
            after_model_callback: Some(Arc::new(|_ctx, res: LlmResponse, _err| {
                Box::pin(async move { (res.clone(), Ok(Some(res))) })
            })),
            ..Default::default()
        },
    );
    let bot = llm_agent(LlmAgentConfig {
        name: "bot".into(),
        model: Some(failing_llm("model failure 503")),
        on_model_error_callbacks: vec![Arc::new(|_ctx, _req, _err| {
            Box::pin(async {
                Ok(Some(LlmResponse {
                    content: Some(text_content("model", "fallback answer")),
                    ..Default::default()
                }))
            })
        })],
        ..Default::default()
    });
    let r = new_test_runner(bot, vec![earlier, tp.plugin.clone()]);
    let mut got_answer = false;
    let (_, errs) = run_until(&r, "u1", "s1", "hi", RunConfig::default(), |item| {
        got_answer = matches!(item, Ok(ev) if ev.response.content.is_some());
        got_answer
    })
    .await;
    assert!(errs.is_empty(), "r.Run error: {errs:?}");
    assert!(got_answer, "r.Run ended without the fallback answer");

    let rows = RowCursor::default()
        .collect(&tp, until_invocation_end)
        .await;
    assert_eq!(
        rows_of_type(&rows, EVENT_LLM_ERROR).len(),
        1,
        "LLM_ERROR rows"
    );
    assert_eq!(
        rows_of_type(&rows, EVENT_LLM_RESPONSE).len(),
        0,
        "LLM_RESPONSE rows: the earlier plugin ends the AfterModelCallback chain"
    );
    require_invocation_completed_ok(&rows);
}

#[tokio::test]
async fn test_runner_event_replacing_plugin_after_analytics_emits_invocation_completed_ok() {
    let tp = setup_test_plugin().await;
    let replacer = plugin_named(
        "event_replacer",
        gadk_agent::PluginConfig {
            on_event_callback: Some(Arc::new(|_ictx, mut ev: gadk_session::Event| {
                ev.response.custom_metadata =
                    json!({"replaced": true}).as_object().cloned().unwrap();
                Box::pin(async move { Ok(Some(ev)) })
            })),
            ..Default::default()
        },
    );
    let custom_bot = custom_agent("custom_bot", vec![], |ctx| {
        let mut ev = new_event(&ctx);
        ev.node_info = Some(gadk_session::NodeInfo {
            path: "custom_bot".into(),
            ..Default::default()
        });
        ev.output = Some(json!({"answer": 42}));
        futures::stream::iter([Ok(ev)]).boxed()
    });
    let r = new_test_runner(custom_bot, vec![tp.plugin.clone(), replacer]);
    let mut got_replaced = false;
    let (_, errs) = run_until(&r, "u1", "s1", "hi", RunConfig::default(), |item| {
        if let Ok(ev) = item
            && ev.output.is_some()
        {
            got_replaced = ev.response.custom_metadata.get("replaced") == Some(&json!(true));
            return true;
        }
        false
    })
    .await;
    assert!(errs.is_empty(), "r.Run error: {errs:?}");
    assert!(
        got_replaced,
        "r.Run ended without the event that the second plugin replaced"
    );

    let rows = RowCursor::default()
        .collect(&tp, until_invocation_end)
        .await;
    assert_eq!(
        rows_of_type(&rows, EVENT_NODE_OUTPUT).len(),
        1,
        "{EVENT_NODE_OUTPUT} rows: the analytics plugin sees the event first"
    );
    require_invocation_completed_ok(&rows);
}

/// Go `runSubAgentFailure(t, childErr, parentResponds)`: a custom parent runs a custom child that
/// writes a state-only event and then fails with `child_err`; with `parent_responds` the parent
/// writes an event with content after each event of the child. Returns the rows and the first
/// error the runner gave its caller.
async fn run_sub_agent_failure(
    child_err: &'static str,
    parent_responds: bool,
) -> (Vec<Row>, Option<AgentError>) {
    let tp = setup_test_plugin().await;
    let child = custom_agent("child", vec![], move |ctx| {
        let mut ev = new_event(&ctx);
        ev.actions.state_delta = Some(json!({"step": 1}).as_object().cloned().unwrap());
        futures::stream::iter([Ok(ev), Err(AgentError::other(child_err))]).boxed()
    });
    let run_child = child.clone();
    let parent = custom_agent("parent", vec![child], move |ctx| {
        let progress_ctx = ctx.clone();
        forward_stream(run_child.run(ctx), move |item| match item {
            Err(e) => (vec![Err(e)], true),
            Ok(ev) => {
                let mut out = vec![Ok(ev)];
                if parent_responds {
                    let mut progress = new_event(&progress_ctx);
                    progress.response.content = Some(text_content("model", "working on it"));
                    out.push(Ok(progress));
                }
                (out, false)
            }
        })
    });
    let r = new_test_runner(parent, vec![tp.plugin.clone()]);
    let (_, mut errs) = run_until_error(&r, "hi").await;
    let rows = RowCursor::default()
        .collect(&tp, until_invocation_end)
        .await;
    let first = if errs.is_empty() {
        None
    } else {
        Some(errs.remove(0))
    };
    (rows, first)
}

#[tokio::test]
async fn test_runner_sub_agent_fails_after_parent_produced_content_emits_invocation_completed_ok() {
    let child_err = "child failure";
    let (rows, got_err) = run_sub_agent_failure(child_err, true).await;
    assert_eq!(
        got_err.map(|e| e.to_string()).as_deref(),
        Some(child_err),
        "r.Run error"
    );
    assert_eq!(
        rows_of_type(&rows, EVENT_AGENT_STARTING).len(),
        2,
        "AGENT_STARTING rows, want 2 (parent and child)"
    );
    require_invocation_completed_ok(&rows);
}

#[tokio::test]
async fn test_runner_sub_agent_fails_without_parent_content_emits_invocation_error() {
    let child_err = "child failure";
    let (rows, got_err) = run_sub_agent_failure(child_err, false).await;
    assert_eq!(
        got_err.map(|e| e.to_string()).as_deref(),
        Some(child_err),
        "r.Run error"
    );
    assert!(
        rows_of_type(&rows, EVENT_INVOCATION_COMPLETED).is_empty(),
        "got INVOCATION_COMPLETED after the sub-agent failure, want INVOCATION_ERROR; event types: {:?}",
        event_types(&rows)
    );
    assert_eq!(
        agent_error_counts(&rows),
        BTreeMap::from([("child".to_owned(), 1), ("parent".to_owned(), 1)]),
        "AGENT_ERROR rows per agent"
    );
    only_row_of_type(&rows, EVENT_INVOCATION_ERROR);
}

#[tokio::test]
async fn test_runner_sub_agent_model_error_caller_keeps_reading_emits_invocation_error() {
    // sequential sub-agent fails after sibling succeeds
    {
        let tp = setup_test_plugin().await;
        let model_err = "subagent model failure 503";
        let ok_child = llm_agent(LlmAgentConfig {
            name: "ok_child".into(),
            model: Some(scripted_llm(vec![final_text("ok")])),
            ..Default::default()
        });
        let fail_child = llm_agent(LlmAgentConfig {
            name: "fail_child".into(),
            model: Some(failing_llm(model_err)),
            ..Default::default()
        });
        let root = sequential("root", vec![ok_child, fail_child]);
        let r = new_test_runner(root, vec![tp.plugin.clone()]);
        let (_, errs) = run_turn(&r, "u1", "s1", "go", RunConfig::default()).await;
        assert!(!errs.is_empty(), "expected runner.Run to yield an error");

        let rows = RowCursor::default()
            .collect(&tp, until_invocation_end)
            .await;
        assert!(
            rows_of_type(&rows, EVENT_INVOCATION_COMPLETED).is_empty(),
            "got INVOCATION_COMPLETED after sub-agent model failure when caller kept reading; event types: {:?}",
            event_types(&rows)
        );
        let inv_row = only_row_of_type(&rows, EVENT_INVOCATION_ERROR);
        assert_eq!(
            str_col(&inv_row, "error_message"),
            model_err,
            "INVOCATION_ERROR error_message"
        );
        assert_eq!(
            agent_error_counts(&rows)
                .get("ok_child")
                .copied()
                .unwrap_or(0),
            0,
            "ok_child AGENT_ERROR rows"
        );
    }

    // loop agent child fails on pass 1 and succeeds on pass 2
    {
        let tp = setup_test_plugin().await;
        let model_err = "pass 1 model failure 503";
        let flaky_child = llm_agent(LlmAgentConfig {
            name: "flaky_child".into(),
            // Go flakyOnFirstPassLLM.
            model: Some(counted_llm("flaky-on-first-pass-llm", move |n| {
                if n == 0 {
                    vec![Err(gadk_model::ModelError::Other(model_err.into()))]
                } else {
                    vec![final_text("recovered on pass 2")]
                }
            })),
            ..Default::default()
        });
        let loop_root = gadk_engine::loop_agent::new(gadk_engine::loop_agent::LoopAgentConfig {
            agent: gadk_agent::AgentConfig {
                name: "loop_root".into(),
                sub_agents: vec![flaky_child],
                ..Default::default()
            },
            max_iterations: 2,
        })
        .unwrap();
        let r = new_test_runner(loop_root, vec![tp.plugin.clone()]);
        let (_, errs) = run_turn(&r, "u1", "s1", "go", RunConfig::default()).await;
        assert!(
            !errs.is_empty(),
            "expected runner.Run to yield an error on pass 1"
        );

        let rows = RowCursor::default()
            .collect(&tp, until_invocation_end)
            .await;
        assert!(
            rows_of_type(&rows, EVENT_INVOCATION_COMPLETED).is_empty(),
            "got INVOCATION_COMPLETED after pass 1 model failure in LoopAgent; event types: {:?}",
            event_types(&rows)
        );
        let inv_row = only_row_of_type(&rows, EVENT_INVOCATION_ERROR);
        assert_eq!(
            str_col(&inv_row, "error_message"),
            model_err,
            "INVOCATION_ERROR error_message"
        );
    }
}

/// The rows of `event_type` per agent, in order.
fn rows_by_agent(rows: &[Row], event_type: &str) -> BTreeMap<String, Vec<Row>> {
    let mut out: BTreeMap<String, Vec<Row>> = BTreeMap::new();
    for row in rows_of_type(rows, event_type) {
        out.entry(agent_of(&row)).or_default().push(row);
    }
    out
}

#[tokio::test]
async fn test_runner_task_mode_delegation_emits_agent_completed_and_retires_first_coordinator_activation()
 {
    let new_task_worker = || {
        llm_agent(LlmAgentConfig {
            name: "task_worker".into(),
            description: "executes delegated tasks".into(),
            mode: gadk_agent::Mode::Task,
            model: Some(scripted_llm(vec![calls(vec![function_call_part_id(
                "call-finish-task",
                "finish_task",
                json!({"result": "delegated task finished"}),
            )])])),
            ..Default::default()
        })
    };
    let delegate_call_response = || {
        calls(vec![function_call_part_id(
            "call-task-worker",
            "task_worker",
            json!({"request": "run the task"}),
        )])
    };

    // happy path
    {
        let tp = setup_test_plugin().await;
        let coordinator = llm_agent(LlmAgentConfig {
            name: "coordinator".into(),
            sub_agents: vec![new_task_worker()],
            model: Some(scripted_llm(vec![
                delegate_call_response(),
                final_text("all done"),
            ])),
            ..Default::default()
        });
        let r = new_test_runner(coordinator, vec![tp.plugin.clone()]);
        let (_, errs) = run_turn(&r, "u1", "s1", "start", RunConfig::default()).await;
        assert!(errs.is_empty(), "r.Run error: {errs:?}");

        let rows = RowCursor::default()
            .collect(&tp, until_invocation_end)
            .await;
        require_invocation_completed_ok(&rows);
        let inv_span = place_of(&only_row_of_type(&rows, EVENT_INVOCATION_STARTING)).span;
        let starts = rows_by_agent(&rows, EVENT_AGENT_STARTING);
        let completes = rows_by_agent(&rows, EVENT_AGENT_COMPLETED);
        let none = Vec::new();
        let coord_starts = starts.get("coordinator").unwrap_or(&none);
        let worker_starts = starts.get("task_worker").unwrap_or(&none);
        let coord_completes = completes.get("coordinator").unwrap_or(&none);
        let worker_completes = completes.get("task_worker").unwrap_or(&none);

        assert_eq!(coord_starts.len(), 2, "coordinator AGENT_STARTING count");
        for (i, cs) in coord_starts.iter().enumerate() {
            assert_eq!(
                place_of(cs).parent,
                inv_span,
                "coordinator AGENT_STARTING[{i}] parent_span_id, want invocation span"
            );
        }
        assert_eq!(worker_starts.len(), 1, "task_worker AGENT_STARTING count");
        assert_eq!(
            place_of(&worker_starts[0]).parent,
            place_of(&coord_starts[0]).span,
            "task_worker AGENT_STARTING parent_span_id, want first coordinator span"
        );
        assert_eq!(
            worker_completes.len(),
            1,
            "task_worker AGENT_COMPLETED count"
        );
        assert_eq!(
            place_of(&worker_completes[0]),
            place_of(&worker_starts[0]),
            "task_worker AGENT_COMPLETED place"
        );
        must_latency(&worker_completes[0]);
        assert_eq!(
            coord_completes.len(),
            2,
            "coordinator AGENT_COMPLETED count"
        );
        for (i, row) in coord_completes.iter().enumerate() {
            assert_eq!(
                place_of(row),
                place_of(&coord_starts[i]),
                "coordinator AGENT_COMPLETED[{i}] place, want coordinator activation {i}"
            );
            must_latency(row);
        }
        assert_agent_spans_closed(&rows);
    }

    // continuation failure after task completion
    {
        let tp = setup_test_plugin().await;
        let cont_err = "continuation model failure";
        let first = delegate_call_response().expect("delegate call");
        let coordinator = llm_agent(LlmAgentConfig {
            name: "coordinator".into(),
            sub_agents: vec![new_task_worker()],
            // Go firstCallResponseThenErrLLM.
            model: Some(counted_llm("first-call-response-then-err-llm", move |n| {
                if n == 0 {
                    vec![Ok(first.clone())]
                } else {
                    vec![Err(gadk_model::ModelError::Other(cont_err.into()))]
                }
            })),
            ..Default::default()
        });
        let r = new_test_runner(coordinator, vec![tp.plugin.clone()]);
        let (_, errs) = run_turn(&r, "u1", "s1", "start", RunConfig::default()).await;
        assert_eq!(
            errs.last().map(ToString::to_string).as_deref(),
            Some(cont_err),
            "r.Run error"
        );

        let rows = RowCursor::default()
            .collect(&tp, until_invocation_end)
            .await;
        only_row_of_type(&rows, EVENT_INVOCATION_ERROR);
        assert_eq!(
            agent_error_counts(&rows),
            BTreeMap::from([("coordinator".to_owned(), 1)]),
            "AGENT_ERROR counts"
        );
    }
}

/// Go `transferModel{script}`: transfers to each name of `script` in turn, then answers "done".
fn transfer_model(script: Vec<&'static str>) -> Arc<dyn gadk_model::Llm> {
    counted_llm("transfer-model", move |i| {
        let part = match script.get(i) {
            Some(target) => function_call_part("transfer_to_agent", json!({"agent_name": target})),
            None => gadk_genai::Part::from_text("done"),
        };
        vec![Ok(LlmResponse {
            content: Some(gadk_genai::Content {
                role: "model".into(),
                parts: vec![part],
            }),
            turn_complete: true,
            ..Default::default()
        })]
    })
}

#[tokio::test]
async fn test_runner_transfer_back_to_running_parent_closes_every_span() {
    let cases: Vec<(&str, Vec<&'static str>, Vec<&'static str>)> = vec![
        ("parent", vec!["planner"], vec![]),
        ("peer", vec!["second"], vec!["first"]),
        (
            "double bounce",
            vec!["second", "second"],
            vec!["first", "first"],
        ),
    ];
    for (name, first_script, second_script) in cases {
        let tp = setup_test_plugin().await;
        let first = llm_agent(LlmAgentConfig {
            name: "first".into(),
            model: Some(transfer_model(first_script)),
            ..Default::default()
        });
        let second = llm_agent(LlmAgentConfig {
            name: "second".into(),
            model: Some(transfer_model(second_script)),
            ..Default::default()
        });
        let planner = llm_agent(LlmAgentConfig {
            name: "planner".into(),
            model: Some(transfer_model(vec!["first"])),
            sub_agents: vec![first, second],
            ..Default::default()
        });
        let r = new_test_runner(planner, vec![tp.plugin.clone()]);
        let (_, errs) = run_turn(&r, "u1", "s1", "hi", RunConfig::default()).await;
        assert!(errs.is_empty(), "{name}: {errs:?}");
        let rows = RowCursor::default()
            .collect(&tp, until_invocation_end)
            .await;
        assert_agent_spans_closed(&rows);
    }
}

#[tokio::test]
async fn test_runner_resumed_task_then_transfer_back_closes_every_span() {
    let call = |name: &str, args: serde_json::Value| {
        calls(vec![function_call_part_id(
            &format!("call-{name}"),
            name,
            args,
        )])
    };
    let text = final_text;
    let task = |name: &str, description: &str, model: Arc<gadk_engine::testutil::MockModel>| {
        llm_agent(LlmAgentConfig {
            name: name.into(),
            description: description.into(),
            mode: gadk_agent::Mode::Task,
            model: Some(model),
            ..Default::default()
        })
    };
    type Setup = Box<
        dyn Fn() -> (
            Arc<dyn gadk_agent::Agent>,
            Vec<(Arc<gadk_engine::testutil::MockModel>, usize)>,
        ),
    >;
    let direct_parent: Setup = Box::new(move || {
        let task_model = scripted_mock(vec![
            text("Which color?"),
            call("finish_task", json!({"result": "blue"})),
        ]);
        let helper_task = task("helper_task", "asks, then finishes", task_model.clone());
        let helper_model = scripted_mock(vec![
            call("helper_task", json!({"request": "pick a color"})),
            call("transfer_to_agent", json!({"agent_name": "coord"})),
        ]);
        let helper = llm_agent(LlmAgentConfig {
            name: "helper".into(),
            description: "delegates".into(),
            sub_agents: vec![helper_task],
            model: Some(helper_model.clone()),
            ..Default::default()
        });
        // Having a task child makes the runner restart at coord on turn 2.
        let coord_task = task("coord_task", "unused", scripted_mock(vec![]));
        let coord_model = scripted_mock(vec![
            call("transfer_to_agent", json!({"agent_name": "helper"})),
            text("waiting for the user"),
            call("transfer_to_agent", json!({"agent_name": "helper"})),
            text("all done"),
        ]);
        let coord = llm_agent(LlmAgentConfig {
            name: "coord".into(),
            sub_agents: vec![coord_task, helper],
            model: Some(coord_model.clone()),
            ..Default::default()
        });
        (
            coord,
            vec![(task_model, 2), (helper_model, 2), (coord_model, 4)],
        )
    });
    let intermediate: Setup = Box::new(move || {
        let task_model = scripted_mock(vec![
            text("Which color?"),
            call("finish_task", json!({"result": "blue"})),
        ]);
        let helper_task = task("helper_task", "asks, then finishes", task_model.clone());
        let helper_model = scripted_mock(vec![
            call("helper_task", json!({"request": "pick a color"})),
            call("transfer_to_agent", json!({"agent_name": "mid"})),
        ]);
        let helper = llm_agent(LlmAgentConfig {
            name: "helper".into(),
            description: "delegates".into(),
            sub_agents: vec![helper_task],
            model: Some(helper_model.clone()),
            ..Default::default()
        });
        let mid_model = scripted_mock(vec![
            call("transfer_to_agent", json!({"agent_name": "helper"})),
            text("mid waiting"),
            call("transfer_to_agent", json!({"agent_name": "helper"})),
            text("mid done"),
        ]);
        let mid = llm_agent(LlmAgentConfig {
            name: "mid".into(),
            description: "intermediate".into(),
            sub_agents: vec![helper],
            model: Some(mid_model.clone()),
            ..Default::default()
        });
        let coord_task = task("coord_task", "unused", scripted_mock(vec![]));
        let coord_model = scripted_mock(vec![
            call("transfer_to_agent", json!({"agent_name": "mid"})),
            call("transfer_to_agent", json!({"agent_name": "mid"})),
        ]);
        let coord = llm_agent(LlmAgentConfig {
            name: "coord".into(),
            sub_agents: vec![coord_task, mid],
            model: Some(coord_model.clone()),
            ..Default::default()
        });
        (
            coord,
            vec![
                (task_model, 2),
                (helper_model, 2),
                (mid_model, 4),
                (coord_model, 2),
            ],
        )
    });
    for (name, setup) in [
        ("direct parent", direct_parent),
        ("intermediate chat agent", intermediate),
    ] {
        let tp = setup_test_plugin().await;
        let (root, models) = setup();
        let r = new_test_runner(root, vec![tp.plugin.clone()]);
        let mut resumed_invocation = String::new();
        for msg in ["start", "blue"] {
            let (events, errs) = run_turn(&r, "u1", "s1", msg, RunConfig::default()).await;
            assert!(errs.is_empty(), "{name}: r.Run: {errs:?}");
            if let Some(ev) = events.last() {
                resumed_invocation = ev.invocation_id.clone();
            }
        }
        for (i, (m, scripted)) in models.iter().enumerate() {
            let used = m.requests().len();
            assert!(
                used >= *scripted,
                "{name}: model {i} has {} unconsumed model responses",
                scripted - used
            );
        }
        assert!(
            !resumed_invocation.is_empty(),
            "{name}: resumed run returned no invocation ID"
        );
        let rows = RowCursor::default()
            .collect(&tp, |rows| {
                rows_of_type(rows, EVENT_INVOCATION_COMPLETED).len()
                    + rows_of_type(rows, EVENT_INVOCATION_ERROR).len()
                    >= 2
            })
            .await;
        let resumed_rows: Vec<Row> = rows
            .into_iter()
            .filter(|r| str_col(r, "invocation_id") == resumed_invocation)
            .collect();
        assert!(
            rows_of_type(&resumed_rows, EVENT_INVOCATION_COMPLETED).len() == 1
                && rows_of_type(&resumed_rows, EVENT_INVOCATION_ERROR).is_empty(),
            "{name}: resumed invocation did not complete successfully; event types: {:?}",
            event_types(&resumed_rows)
        );
        assert_agent_spans_closed(&resumed_rows);
    }
}

#[tokio::test]
async fn test_runner_first_streamed_chunk_rejected_by_agent_callback_emits_error_terminal_rows() {
    let cases = vec![
        (
            "generated content chunk rejected by AfterModelCallback",
            LlmResponse {
                partial: true,
                content: Some(text_content("model", "hello ")),
                ..Default::default()
            },
        ),
        (
            "metadata-only leading chunk rejected by AfterModelCallback",
            LlmResponse {
                partial: true,
                usage_metadata: Some(gadk_genai::GenerateContentResponseUsageMetadata {
                    prompt_token_count: 5,
                    ..Default::default()
                }),
                ..Default::default()
            },
        ),
    ];
    for (name, chunk) in cases {
        let tp = setup_test_plugin().await;
        let bot = llm_agent(LlmAgentConfig {
            name: "bot".into(),
            // Go singlePartialChunkLLM.
            model: Some(fn_llm("single-partial-chunk-llm", move |_, _| {
                futures::stream::iter([Ok(chunk.clone())]).boxed()
            })),
            after_model_callbacks: vec![Arc::new(|_ctx, res, _err| {
                Box::pin(
                    async move { (res, Err(AgentError::other("policy rejected first chunk"))) },
                )
            })],
            ..Default::default()
        });
        let r = new_test_runner(bot, vec![tp.plugin.clone()]);
        let cfg = RunConfig {
            streaming_mode: gadk_agent::StreamingMode::Sse,
            ..Default::default()
        };
        let (events, errs) = run_turn(&r, "u1", "s1", "hi", cfg).await;
        assert!(
            !errs.is_empty(),
            "{name}: expected runner.Run to yield AfterModelCallback error"
        );
        assert!(
            !events.iter().any(|ev| ev.response.partial),
            "{name}: did not expect a rejected partial chunk to be yielded as a session event"
        );

        let rows = RowCursor::default()
            .collect(&tp, until_invocation_end)
            .await;
        assert!(
            rows_of_type(&rows, EVENT_INVOCATION_COMPLETED).is_empty(),
            "{name}: got INVOCATION_COMPLETED rows, want 0; event types: {:?}",
            event_types(&rows)
        );
        let inv_err = only_row_of_type(&rows, EVENT_INVOCATION_ERROR);
        assert_status(&inv_err, "ERROR", None);
        assert_eq!(
            agent_error_counts(&rows),
            BTreeMap::from([("bot".to_owned(), 1)]),
            "{name}: AGENT_ERROR counts"
        );
    }
}

#[tokio::test]
async fn test_runner_partial_event_from_custom_root_then_sub_agent_error_emits_error_terminal_rows()
{
    for stop_at_error in [true, false] {
        let tp = setup_test_plugin().await;
        let sub_bot = llm_agent(LlmAgentConfig {
            name: "sub_bot".into(),
            model: Some(scripted_llm(vec![final_text("unused")])),
            before_model_callbacks: vec![Arc::new(|_ctx, req| {
                Box::pin(async move { (req, Err(AgentError::other("sub_bot before-model boom"))) })
            })],
            ..Default::default()
        });
        let run_sub = sub_bot.clone();
        let custom_root = custom_agent("custom_root", vec![sub_bot], move |ctx| {
            let mut partial = new_event(&ctx);
            partial.author = "custom_root".into();
            partial.branch = ctx.branch();
            partial.response = LlmResponse {
                partial: true,
                content: Some(text_content("model", "progress update...")),
                ..Default::default()
            };
            let sub = run_sub.clone();
            let rest = futures::stream::once(async move { sub.run(ctx) }).flatten();
            let head = futures::stream::iter([Ok(partial)]);
            // Go returns after the first error of the sub-agent.
            forward_stream(head.chain(rest).boxed(), |item| {
                let stop = item.is_err();
                (vec![item], stop)
            })
        });
        let r = new_test_runner(custom_root, vec![tp.plugin.clone()]);
        let mut saw_partial = false;
        let mut saw_err = false;
        run_until(
            &r,
            "u1",
            "s1",
            "go",
            RunConfig::default(),
            |item| match item {
                Err(_) => {
                    saw_err = true;
                    stop_at_error
                }
                Ok(ev) => {
                    saw_partial |= ev.response.partial;
                    false
                }
            },
        )
        .await;
        assert!(
            saw_partial && saw_err,
            "stop at error {stop_at_error}: runner.Run yielded partial event {saw_partial} and error {saw_err}, want both"
        );

        let rows = RowCursor::default()
            .collect(&tp, until_invocation_end)
            .await;
        assert!(
            rows_of_type(&rows, EVENT_INVOCATION_COMPLETED).is_empty(),
            "stop at error {stop_at_error}: got INVOCATION_COMPLETED rows, want 0; event types: {:?}",
            event_types(&rows)
        );
        only_row_of_type(&rows, EVENT_INVOCATION_ERROR);
        assert_eq!(
            agent_error_counts(&rows).get("sub_bot").copied(),
            Some(1),
            "stop at error {stop_at_error}: AGENT_ERROR rows per agent = {:?}, want one for sub_bot",
            agent_error_counts(&rows)
        );
    }
}

#[tokio::test]
async fn test_runner_single_turn_tool_answered_by_before_agent_callback_emits_no_agent_error() {
    let helper_call = |id: &str| function_call_part_id(id, "helper", json!({"request": "cached?"}));
    let cases = vec![
        ("one call", vec![helper_call("call-helper-1")]),
        (
            "two calls in one turn",
            vec![helper_call("call-helper-1"), helper_call("call-helper-2")],
        ),
    ];
    for (name, parts) in cases {
        let tp = setup_test_plugin().await;
        let helper = llm_agent(LlmAgentConfig {
            name: "helper".into(),
            description: "Answers from a cache.".into(),
            mode: gadk_agent::Mode::SingleTurn,
            model: Some(scripted_llm(vec![])),
            before_agent_callbacks: vec![answering_before_agent(text_content(
                "model",
                "cached answer",
            ))],
            ..Default::default()
        });
        let coordinator = llm_agent(LlmAgentConfig {
            name: "coordinator".into(),
            mode: gadk_agent::Mode::Chat,
            sub_agents: vec![helper],
            model: Some(scripted_llm(vec![
                calls(parts),
                final_text("done with the cached answer"),
            ])),
            ..Default::default()
        });
        let r = new_test_runner(coordinator, vec![tp.plugin.clone()]);
        let (_, errs) = run_turn(&r, "u1", "s1", "help me", RunConfig::default()).await;
        assert!(
            errs.is_empty(),
            "{name}: r.Run yielded unexpected error: {errs:?}"
        );

        let rows = RowCursor::default()
            .collect(&tp, until_invocation_end)
            .await;
        assert_eq!(
            agent_error_counts(&rows)
                .get("helper")
                .copied()
                .unwrap_or(0),
            0,
            "{name}: AGENT_ERROR rows for helper; event types: {:?}",
            event_types(&rows)
        );
        assert!(
            rows_of_type(&rows, EVENT_INVOCATION_ERROR).is_empty(),
            "{name}: got INVOCATION_ERROR, want INVOCATION_COMPLETED; event types: {:?}",
            event_types(&rows)
        );
        only_row_of_type(&rows, EVENT_INVOCATION_COMPLETED);
    }
}

/// The `timestamp` column of `row`.
fn timestamp_of(row: &Row) -> chrono::DateTime<chrono::Utc> {
    match row.get("timestamp") {
        Some(crate::row::RowValue::Timestamp(ts)) => *ts,
        other => panic!("timestamp is {other:?}, want a timestamp"),
    }
}

// parity: adapted: Go's `time.Sleep` in the custom agents is a real-time `tokio::time::sleep` (the
// rows' timestamps are wall-clock, so paused time cannot advance them).
#[tokio::test]
async fn test_runner_agent_completed_and_end_of_run_error_timestamps_are_not_backdated() {
    let min_gap = chrono::Duration::milliseconds(20);
    // state_delta_and_agent_completed
    {
        let tp = setup_test_plugin().await;
        let step_delay = std::time::Duration::from_millis(25);
        let root = custom_agent("custom_ok", vec![], move |ctx| {
            let head = futures::stream::once(async move {
                tokio::time::sleep(step_delay).await;
                let mut ev = new_event(&ctx);
                ev.actions.state_delta =
                    Some(json!({"step": "done"}).as_object().cloned().unwrap());
                Ok(ev)
            });
            let tail = futures::stream::once(async move {
                tokio::time::sleep(step_delay).await;
                None
            })
            .filter_map(|x: Option<Result<gadk_session::Event, AgentError>>| async move { x });
            head.chain(tail).boxed()
        });
        let r = new_test_runner(root, vec![tp.plugin.clone()]);
        let (_, errs) = run_turn(&r, "u1", "s1", "go", RunConfig::default()).await;
        assert!(errs.is_empty(), "r.Run yielded unexpected error: {errs:?}");
        let rows = RowCursor::default()
            .collect(&tp, until_invocation_end)
            .await;
        only_row_of_type(&rows, EVENT_INVOCATION_COMPLETED);
        let start_ts = timestamp_of(&only_row_of_type(&rows, EVENT_AGENT_STARTING));
        let delta_ts = timestamp_of(&only_row_of_type(&rows, EVENT_STATE_DELTA));
        let completed_ts = timestamp_of(&only_row_of_type(&rows, EVENT_AGENT_COMPLETED));
        assert!(
            delta_ts - start_ts >= min_gap,
            "STATE_DELTA timestamp - AGENT_STARTING timestamp = {}, want >= 20ms (not backdated to AGENT_STARTING)",
            delta_ts - start_ts
        );
        assert!(
            completed_ts - delta_ts >= min_gap,
            "AGENT_COMPLETED timestamp - STATE_DELTA timestamp = {}, want >= 20ms (not backdated to lastActivity)",
            completed_ts - delta_ts
        );
    }

    // end_of_run_agent_error
    {
        let tp = setup_test_plugin().await;
        let fail_delay = std::time::Duration::from_millis(25);
        let root = custom_agent("custom_fail", vec![], move |_ctx| {
            futures::stream::once(async move {
                tokio::time::sleep(fail_delay).await;
                Err(AgentError::other("custom agent boom"))
            })
            .boxed()
        });
        let r = new_test_runner(root, vec![tp.plugin.clone()]);
        let (_, errs) = run_until_error(&r, "go").await;
        assert!(
            !errs.is_empty(),
            "r.Run yielded nil error, want custom agent boom"
        );
        let rows = RowCursor::default()
            .collect(&tp, until_invocation_end)
            .await;
        only_row_of_type(&rows, EVENT_INVOCATION_ERROR);
        let start_ts = timestamp_of(&only_row_of_type(&rows, EVENT_AGENT_STARTING));
        let err_ts = timestamp_of(&only_row_of_type(&rows, EVENT_AGENT_ERROR));
        assert!(
            err_ts - start_ts >= min_gap,
            "end-of-run AGENT_ERROR timestamp - AGENT_STARTING timestamp = {}, want >= 20ms (not backdated to lastActivity)",
            err_ts - start_ts
        );
    }
}

#[tokio::test]
async fn test_runner_model_canceled_own_request_after_partial_emits_error_terminal_rows() {
    let tp = setup_test_plugin().await;
    // Go selfCancelingStreamLLM: one partial chunk, then the error of a request context it
    // canceled itself (Go context.Canceled = gadk AgentError::Cancelled).
    let model = fn_llm("self-canceling-stream-llm", |_, _| {
        futures::stream::iter([
            partial_text("hello "),
            Err(gadk_model::ModelError::Other(Box::new(
                AgentError::Cancelled,
            ))),
        ])
        .boxed()
    });
    let bot = llm_agent(LlmAgentConfig {
        name: "bot".into(),
        model: Some(model),
        ..Default::default()
    });
    let seq = sequential("seq_root", vec![bot]);
    let r = new_test_runner(seq, vec![tp.plugin.clone()]);
    let cfg = RunConfig {
        streaming_mode: gadk_agent::StreamingMode::Sse,
        ..Default::default()
    };
    let (events, errs) = run_turn(&r, "u1", "s1", "stream please", cfg).await;
    let saw_partial = events.iter().any(|ev| ev.response.partial);
    let got_err = errs.last().map(ToString::to_string);
    assert!(
        saw_partial && got_err.as_deref() == Some("context canceled"),
        "runner.Run yielded partial event {saw_partial} and error {got_err:?}, want a partial event and context.Canceled"
    );

    let rows = RowCursor::default()
        .collect(&tp, until_invocation_end)
        .await;
    assert!(
        rows_of_type(&rows, EVENT_INVOCATION_COMPLETED).is_empty(),
        "got INVOCATION_COMPLETED rows, want 0; event types: {:?}",
        event_types(&rows)
    );
    only_row_of_type(&rows, EVENT_INVOCATION_ERROR);
}

#[tokio::test]
async fn test_runner_progress_event_without_content() {
    let new_pinger = |name: &str| {
        custom_agent(name, vec![], |ctx| {
            let mut ping = new_event(&ctx);
            ping.branch = ctx.branch();
            ping.response.partial = true;
            futures::stream::iter([Ok(ping)]).boxed()
        })
    };
    let fail_err = "failer failed";
    let failer = custom_agent("failer", vec![], move |_ctx| {
        futures::stream::iter([Err(AgentError::other(fail_err))]).boxed()
    });

    // caller stops on the event
    {
        let tp = setup_test_plugin().await;
        let r = new_test_runner(new_pinger("worker"), vec![tp.plugin.clone()]);
        let mut saw_partial = false;
        let (_, errs) = run_until(&r, "u1", "s1", "go", RunConfig::default(), |item| {
            saw_partial = matches!(item, Ok(ev) if ev.response.partial);
            saw_partial
        })
        .await;
        assert!(errs.is_empty(), "r.Run error: {errs:?}");
        assert!(saw_partial, "runner.Run yielded no partial event");
        require_invocation_completed_ok(
            &RowCursor::default()
                .collect(&tp, until_invocation_end)
                .await,
        );
    }

    // sequential sibling fails after the event
    {
        let tp = setup_test_plugin().await;
        let seq = sequential("seq_root", vec![new_pinger("pinger"), failer]);
        let r = new_test_runner(seq, vec![tp.plugin.clone()]);
        let (_, errs) = run_until_error(&r, "go").await;
        assert_eq!(
            errs.first().map(ToString::to_string).as_deref(),
            Some(fail_err),
            "runner.Run error"
        );
        let rows = RowCursor::default()
            .collect(&tp, until_invocation_end)
            .await;
        assert!(
            rows_of_type(&rows, EVENT_INVOCATION_COMPLETED).is_empty(),
            "got INVOCATION_COMPLETED rows, want 0; event types: {:?}",
            event_types(&rows)
        );
        only_row_of_type(&rows, EVENT_INVOCATION_ERROR);
        assert_eq!(
            agent_error_counts(&rows).get("failer").copied(),
            Some(1),
            "AGENT_ERROR rows per agent = {:?}, want one for failer",
            agent_error_counts(&rows)
        );
    }
}

/// Go `newCacheHitAgent(t)`: an LLM agent `cached` that its BeforeAgentCallback answers.
fn new_cache_hit_agent() -> Arc<dyn gadk_agent::Agent> {
    llm_agent(LlmAgentConfig {
        name: "cached".into(),
        model: Some(never_sent()),
        before_agent_callbacks: vec![answering_before_agent(text_content("model", "cache hit"))],
        ..Default::default()
    })
}

/// Go's `guard`: a custom agent that drains `cached` without forwarding its events.
fn silent_guard(cached: Arc<dyn gadk_agent::Agent>) -> Arc<dyn gadk_agent::Agent> {
    let run_cached = cached.clone();
    custom_agent("guard", vec![cached], move |ctx| {
        drain_then(run_cached.clone(), ctx, |_| Vec::new())
    })
}

#[tokio::test]
async fn test_runner_custom_parent_over_sequence_with_cached_step_emits_invocation_completed() {
    // custom_parent_over_sequence_with_own_answer
    {
        let tp = setup_test_plugin().await;
        let seq = sequential("seq", vec![new_cache_hit_agent()]);
        let run_seq = seq.clone();
        let parent = custom_agent("parent", vec![seq], move |ctx| {
            drain_then(run_seq.clone(), ctx, |ctx| {
                let mut ev = new_event(ctx);
                ev.response.content = Some(text_content("model", "parent answer"));
                vec![Ok(ev)]
            })
        });
        let r = new_test_runner(parent, vec![tp.plugin.clone()]);
        let (_, errs) = run_turn(&r, "u1", "s1", "hi", RunConfig::default()).await;
        assert!(errs.is_empty(), "r.Run error: {errs:?}");
        require_invocation_completed_ok(
            &RowCursor::default()
                .collect(&tp, until_invocation_end)
                .await,
        );
    }

    // guard_over_cached_without_own_event
    {
        let tp = setup_test_plugin().await;
        let guard = silent_guard(new_cache_hit_agent());
        let r = new_test_runner(guard, vec![tp.plugin.clone()]);
        let (_, errs) = run_turn(&r, "u1", "s1", "hi", RunConfig::default()).await;
        assert!(errs.is_empty(), "r.Run error: {errs:?}");
        require_invocation_completed_ok(
            &RowCursor::default()
                .collect(&tp, until_invocation_end)
                .await,
        );
    }

    // seq_root_with_silent_guard_over_cached_then_final_llm
    {
        let tp = setup_test_plugin().await;
        let guard = silent_guard(new_cache_hit_agent());
        let final_llm = llm_agent(LlmAgentConfig {
            name: "final_llm".into(),
            model: Some(scripted_llm(vec![final_text("done")])),
            ..Default::default()
        });
        let seq_root = sequential("seq_root", vec![guard, final_llm]);
        let r = new_test_runner(seq_root, vec![tp.plugin.clone()]);
        let (_, errs) = run_turn(&r, "u1", "s1", "hi", RunConfig::default()).await;
        assert!(errs.is_empty(), "r.Run error: {errs:?}");
        require_invocation_completed_ok(
            &RowCursor::default()
                .collect(&tp, until_invocation_end)
                .await,
        );
    }
}

#[tokio::test]
async fn test_trace_manager_custom_child_ends_own_invocation_after_event() {
    let tp = setup_test_plugin().await;
    let gate = custom_agent("gate", vec![], |ctx| {
        let mut state = new_event(&ctx);
        state.author = "gate".into();
        state.actions.state_delta = Some(json!({"gate": "closed"}).as_object().cloned().unwrap());
        futures::stream::iter([Ok(state)])
            .chain(run_then(move || ctx.end_invocation()))
            .boxed()
    });
    let run_gate = gate.clone();
    let parent = custom_agent("parent", vec![gate], move |ctx| run_gate.run(ctx));
    let r = new_test_runner(parent, vec![tp.plugin.clone()]);
    let (_, errs) = run_turn(&r, "u1", "s1", "go", RunConfig::default()).await;
    assert!(errs.is_empty(), "runner.Run error: {errs:?}");
    require_invocation_completed_ok(
        &RowCursor::default()
            .collect(&tp, until_invocation_end)
            .await,
    );
}

#[tokio::test]
async fn test_failed_agent_root_fails_after_contentless_partial_and_completed_child() {
    let tp = setup_test_plugin().await;
    let child = llm_agent(LlmAgentConfig {
        name: "child".into(),
        model: Some(scripted_llm(vec![final_text("child done")])),
        ..Default::default()
    });
    let root_err = "root failed after child completed";
    let run_child = child.clone();
    let root = custom_agent("root", vec![child], move |ctx| {
        let mut partial = new_event(&ctx);
        partial.author = "root".into();
        partial.branch = ctx.branch();
        partial.response.partial = true;
        // Drain child without forwarding its events.
        let rest = drain_then(run_child.clone(), ctx, move |_| {
            vec![Err(AgentError::other(root_err))]
        });
        futures::stream::iter([Ok(partial)]).chain(rest).boxed()
    });
    let r = new_test_runner(root, vec![tp.plugin.clone()]);
    let (_, errs) = run_until_error(&r, "go").await;
    assert_eq!(
        errs.first().map(ToString::to_string).as_deref(),
        Some(root_err),
        "runner.Run error"
    );
    let rows = RowCursor::default()
        .collect(&tp, until_invocation_end)
        .await;
    only_row_of_type(&rows, EVENT_INVOCATION_ERROR);
    assert!(
        rows_of_type(&rows, EVENT_INVOCATION_COMPLETED).is_empty(),
        "got INVOCATION_COMPLETED rows, want 0; event types: {:?}",
        event_types(&rows)
    );
    assert_eq!(
        agent_error_counts(&rows),
        BTreeMap::from([("root".to_owned(), 1)]),
        "AGENT_ERROR counts; event types: {:?}",
        event_types(&rows)
    );
    assert_eq!(
        agent_rows_of_type(&rows, EVENT_AGENT_COMPLETED, "child").len(),
        1,
        "AGENT_COMPLETED rows of child; event types: {:?}",
        event_types(&rows)
    );
}

#[tokio::test]
async fn test_runner_non_task_mode_finish_task_tool() {
    let new_custom_finish_task_tool = || {
        function_tool(
            "finish_task",
            "Custom non-task finish tool",
            |_ctx, _args| async move {
                Ok(json!({"result": "Task completed."})
                    .as_object()
                    .cloned()
                    .unwrap())
            },
        )
    };

    // chat-mode root agent with custom finish_task tool
    {
        let tp = setup_test_plugin().await;
        let bot = llm_agent(LlmAgentConfig {
            name: "chat_bot".into(),
            tools: vec![new_custom_finish_task_tool()],
            model: Some(scripted_llm(vec![
                calls(vec![function_call_part_id(
                    "call-custom-finish",
                    "finish_task",
                    json!({}),
                )]),
                final_text("finished normally"),
            ])),
            ..Default::default()
        });
        let r = new_test_runner(bot, vec![tp.plugin.clone()]);
        let (_, errs) = run_turn(&r, "u1", "s1", "go", RunConfig::default()).await;
        assert!(errs.is_empty(), "r.Run error: {errs:?}");
        let rows = RowCursor::default()
            .collect(&tp, until_invocation_end)
            .await;
        require_invocation_completed_ok(&rows);
        assert_eq!(
            rows_of_type(&rows, EVENT_AGENT_COMPLETED).len(),
            1,
            "AGENT_COMPLETED rows; event types: {:?}",
            event_types(&rows)
        );
        assert_agent_spans_closed(&rows);
    }

    // single-turn sub-agent with custom finish_task tool
    {
        let tp = setup_test_plugin().await;
        let helper = llm_agent(LlmAgentConfig {
            name: "single_turn_helper".into(),
            description: "Runs in single-turn mode".into(),
            mode: gadk_agent::Mode::SingleTurn,
            tools: vec![new_custom_finish_task_tool()],
            model: Some(scripted_llm(vec![
                calls(vec![function_call_part_id(
                    "call-helper-finish",
                    "finish_task",
                    json!({}),
                )]),
                final_text("helper summary"),
            ])),
            ..Default::default()
        });
        let coord = llm_agent(LlmAgentConfig {
            name: "coord".into(),
            sub_agents: vec![helper],
            model: Some(scripted_llm(vec![
                calls(vec![function_call_part_id(
                    "call-helper",
                    "single_turn_helper",
                    json!({"request": "do work"}),
                )]),
                final_text("coord done"),
            ])),
            ..Default::default()
        });
        let r = new_test_runner(coord, vec![tp.plugin.clone()]);
        let (_, errs) = run_turn(&r, "u1", "s1", "go", RunConfig::default()).await;
        assert!(errs.is_empty(), "r.Run error: {errs:?}");
        let rows = RowCursor::default()
            .collect(&tp, until_invocation_end)
            .await;
        require_invocation_completed_ok(&rows);
        assert_agent_spans_closed(&rows);
    }
}

// parity: adapted: Go's `context.WithCancelCause(ctx)` + `cancel(runErr)` is a child cancellation
// token cancelled on the first error (a gadk token carries no cause; spec R3).
#[tokio::test]
async fn test_custom_fan_out_handles_canceled_child() {
    let a = custom_agent("a", vec![], |ctx| {
        let mut ev = new_event(&ctx);
        ev.author = "a".into();
        ev.branch = ctx.branch();
        ev.response.content = Some(text_content("model", "partial answer"));
        futures::stream::iter([Ok(ev), Err(AgentError::other("errA"))]).boxed()
    });
    let run_a = a.clone();
    let root = custom_agent("fanout", vec![a], move |ctx| {
        let token = ctx.cancellation_token().child_token();
        let gctx = ctx.with_delta(gadk_agent::ContextDelta {
            cancellation_token: Some(token.clone()),
            ..Default::default()
        });
        let fallback_ctx = ctx.clone();
        let fallback = futures::stream::once(async move {
            let mut ev = new_event(&fallback_ctx);
            ev.author = "fanout".into();
            ev.branch = fallback_ctx.branch();
            ev.response.content = Some(text_content("model", "fallback answer"));
            Ok(ev)
        });
        let guard = token.clone().drop_guard();
        forward_stream(run_a.run(gctx), move |item| match item {
            Err(_) => {
                // as errgroup.WithContext does on the first error
                token.cancel();
                (Vec::new(), true)
            }
            Ok(ev) => (vec![Ok(ev)], false),
        })
        .chain(fallback)
        .chain(run_then(move || drop(guard)))
        .boxed()
    });
    let tp = setup_test_plugin().await;
    let r = new_test_runner(root, vec![tp.plugin.clone()]);
    let (_, errs) = run_turn(&r, "u1", "s1", "hello", RunConfig::default()).await;
    assert!(errs.is_empty(), "runner.Run yielded an error: {errs:?}");
    let rows = RowCursor::default()
        .collect(&tp, until_invocation_end)
        .await;
    assert_eq!(
        rows_of_type(&rows, EVENT_INVOCATION_ERROR).len(),
        0,
        "INVOCATION_ERROR rows"
    );
}
