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
//! groups B, F and H: direct trace-manager tests with goroutine information set by hand,
//! agent-tool helper runs on the tool's goroutine, and runner-level tests that need the
//! consumer's goroutine (plus the early-stop and sequential-root runner tests that need only it).
//! Shared helpers: [`attr_support`](super::attr_support).
//!
//! Go → gadk: the test goroutine of a direct test that compares records with
//! `currentGoroutineID()` = [`on_test_goroutine`] / [`goroutine_id`]; `newHelperCoordinator` /
//! `runHelperCoordinator` = [`new_helper_coordinator`] / [`run_helper_coordinator`]; the late-review
//! helpers of `direct_4` are repeated here (private there).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use gadk_agent::{Agent, AgentError, ContextDelta, InvocationContext, RunConfig, Tool};
use gadk_engine::llm::LlmAgentConfig;
use gadk_genai::{FunctionCall, Part};
use gadk_model::{LlmResponse, ModelError};
use parking_lot::Mutex;
use serde_json::{Map, Value, json};

use super::attr_support::{
    Gate, context_canceled_on_done_llm, goroutine_id, on_test_goroutine, self_canceling_stream_llm,
    stream_then_wait_llm, then_wait_for_cancel,
};
use super::direct_support::{
    bg, check_late_record_dropped, ended_ctx, late_review_agent, late_review_stack,
    late_review_trace,
};
use super::runner_support::{
    RowCursor, calls, custom_agent, failing_llm, final_text, fn_llm, function_call_part,
    function_tool, llm_agent, partial_text, plugin_named,
    require_completed_with_handled_agent_error, run_until, scripted_llm, sse,
};
use crate::event_types::*;
use crate::parity_tests::plugin_agentanalytics_bigquery_agent_analytics_plugin::support::{
    agent_error_counts, default_config, event_types, new_test_runner, new_trace_only_plugin,
    only_row_of_type, require_invocation_completed_ok, rows_of_type, run_turn, setup_test_plugin,
    str_col, text_content, until_invocation_end,
};
use crate::plugin::AnalyticsPlugin;
use crate::plugin::callbacks::callbacks_config;
use crate::row::Row;
use crate::trace::span::{AttemptSite, GoroutineOrigin, NodeRunKey};
use crate::trace::testkit::{custom, ident};
use crate::trace::{
    InvocationTrace, NO_FRAME, SpanKind, SpanRecord, SpanRef, TraceManager, new_span_id, tool_key,
};

// ---------------------------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------------------------

/// A model response calling `name` with ID `id` and `args` (Go `&genai.FunctionCall{ID, Name,
/// Args}` in a model content).
fn call_part(id: &str, name: &str, args: Value) -> Part {
    Part {
        function_call: Some(FunctionCall {
            id: id.into(),
            name: name.into(),
            args: args.as_object().cloned().unwrap_or_default(),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// Go `newHelperCoordinator(t, helperModel)`: a chat coordinator that calls its single-turn
/// sub-agent `helper` (model `helper_model`) as a tool once, then answers.
fn new_helper_coordinator(helper_model: Arc<dyn gadk_model::Llm>) -> Arc<dyn Agent> {
    let helper = llm_agent(LlmAgentConfig {
        name: "helper".into(),
        description: "Runs a single-turn helper task.".into(),
        mode: gadk_agent::Mode::SingleTurn,
        model: Some(helper_model),
        ..Default::default()
    });
    llm_agent(LlmAgentConfig {
        name: "coordinator".into(),
        mode: gadk_agent::Mode::Chat,
        sub_agents: vec![helper],
        model: Some(scripted_llm(vec![
            calls(vec![call_part(
                "call-helper-1",
                "helper",
                json!({"request": "stream something"}),
            )]),
            final_text("done after the helper"),
        ])),
        ..Default::default()
    })
}

/// Whether `item` is a partial event of `author`.
fn is_partial_of(item: &Result<gadk_session::Event, AgentError>, author: &str) -> bool {
    matches!(item, Ok(ev) if ev.response.partial && ev.author == author)
}

/// Go `runHelperCoordinator(t, helperModel, stop)`: runs [`new_helper_coordinator`] streaming,
/// stopping on the helper's first partial event if `stop`, and returns the rows of the run.
async fn run_helper_coordinator(helper_model: Arc<dyn gadk_model::Llm>, stop: bool) -> Vec<Row> {
    let tp = setup_test_plugin().await;
    let r = new_test_runner(
        new_helper_coordinator(helper_model),
        vec![tp.plugin.clone()],
    );
    let mut saw_partial = false;
    let (_, errs) = run_until(&r, "u1", "s1", "help me", sse(), |item| {
        let partial = is_partial_of(item, "helper");
        saw_partial |= partial;
        partial && stop
    })
    .await;
    assert!(errs.is_empty(), "r.Run error: {errs:?}");
    assert!(saw_partial, "runner.Run yielded no partial event of helper");
    RowCursor::default()
        .collect(&tp, until_invocation_end)
        .await
}

/// Waits (up to 5 s) until `tm` tracks no live invocation: the after-run callbacks of the
/// streams dropped early have run (gadk runs them on a task of their own; Go on the consumer's
/// goroutine, before `break` returns).
async fn wait_for_after_runs(tm: &TraceManager) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !tm.mu.lock().invocations.is_empty() {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("the after-run callbacks of the stopped runs did not end the runs");
}

// ---------------------------------------------------------------------------------------------
// Ports (Go line order)
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn test_runner_after_tool_short_circuit_early_stop_does_not_fail() {
    for (name, earlier_result, want_tool_completed) in [
        (
            "earlier plugin replaces the tool result",
            Some(json!({"status": "replaced"})),
            0,
        ),
        ("earlier plugin leaves the tool result", None, 1),
    ] {
        let tp = setup_test_plugin().await;
        let earlier_result: Option<Map<String, Value>> =
            earlier_result.and_then(|v| v.as_object().cloned());
        let earlier = plugin_named(
            "earlier_after_tool",
            gadk_agent::PluginConfig {
                after_tool_callback: Some(Arc::new(move |_ctx, _tool, _args, _res, _err| {
                    let r = earlier_result.clone();
                    Box::pin(async move { Ok(r) })
                })),
                ..Default::default()
            },
        );
        let lookup = function_tool("lookup", "looks up a value", |_ctx, _args| async {
            Ok(Map::new())
        });
        let bot = llm_agent(LlmAgentConfig {
            name: "bot".into(),
            model: Some(scripted_llm(vec![
                calls(vec![function_call_part("lookup", json!({}))]),
                Ok(LlmResponse {
                    content: Some(text_content("model", "all done")),
                    ..Default::default()
                }),
            ])),
            tools: vec![lookup],
            ..Default::default()
        });
        let r = new_test_runner(bot, vec![earlier, tp.plugin.clone()]);
        let (events, errs) =
            run_until(&r, "u1", "s1", "look it up", RunConfig::default(), |item| {
                matches!(item, Ok(ev) if ev
                .response
                .content
                .as_ref()
                .and_then(|c| c.parts.first())
                .is_some_and(|p| p.text == "all done"))
            })
            .await;
        assert!(errs.is_empty(), "{name}: r.Run error: {errs:?}");
        assert_eq!(
            events.len(),
            3,
            "{name}: consumed events, want 3: the function call, its response and the final answer"
        );

        let rows = RowCursor::default()
            .collect(&tp, until_invocation_end)
            .await;
        assert_eq!(
            rows_of_type(&rows, EVENT_TOOL_STARTING).len(),
            1,
            "{name}: TOOL_STARTING rows"
        );
        assert_eq!(
            rows_of_type(&rows, EVENT_TOOL_COMPLETED).len(),
            want_tool_completed,
            "{name}: TOOL_COMPLETED rows"
        );
        require_invocation_completed_ok(&rows);
    }
}

#[tokio::test]
async fn test_runner_stop_on_tool_run_helper_partial_event_emits_no_agent_error() {
    let rows = run_helper_coordinator(stream_then_wait_llm(None), true).await;
    assert!(
        rows_of_type(&rows, EVENT_LLM_ERROR)
            .iter()
            .any(|r| str_col(r, "agent") == "helper"),
        "got no LLM_ERROR row of helper, want the model call that the stop canceled; event types: {:?}",
        event_types(&rows)
    );
    require_invocation_completed_ok(&rows);
}

#[tokio::test]
async fn test_runner_tool_run_helper_canceled_itself_after_partial_emits_agent_error() {
    let rows = run_helper_coordinator(self_canceling_stream_llm(), false).await;
    require_completed_with_handled_agent_error(&rows, "helper", "context canceled");
}

// parity: adapted: Go cancels the caller's `ctx` passed to r.Run; a gadk run has no caller context, so a plugin's BeforeRunCallback keeps the invocation's context and the consumer cancels it (what the caller's cancellation reaches)
#[tokio::test]
async fn test_runner_tool_run_helper_canceled_by_outer_ctx_after_partial_emits_agent_error() {
    let tp = setup_test_plugin().await;
    let outer: Arc<Mutex<Option<InvocationContext>>> = Arc::default();
    let keeper = plugin_named(
        "outer_ctx",
        gadk_agent::PluginConfig {
            before_run_callback: Some({
                let outer = outer.clone();
                Arc::new(move |ictx: InvocationContext| {
                    *outer.lock() = Some(ictx);
                    Box::pin(async { Ok(None) })
                })
            }),
            ..Default::default()
        },
    );
    let coordinator = new_helper_coordinator(stream_then_wait_llm(None));
    let r = new_test_runner(coordinator, vec![tp.plugin.clone(), keeper]);
    run_until(&r, "u1", "s1", "help me", sse(), |item| {
        if is_partial_of(item, "helper") {
            let ictx = outer.lock().clone().expect("the run's context");
            ictx.cancellation_token().cancel();
        }
        item.is_err()
    })
    .await;
    let rows = RowCursor::default()
        .collect(&tp, until_invocation_end)
        .await;
    // The helper must get an AGENT_ERROR row and the run must end INVOCATION_ERROR: canceling
    // the outer run context is a failure of the run, not a consumer stop.
    assert_eq!(
        agent_error_counts(&rows)
            .get("helper")
            .copied()
            .unwrap_or(0),
        1,
        "AGENT_ERROR rows for helper; event types: {:?}",
        event_types(&rows)
    );
    assert!(
        rows_of_type(&rows, EVENT_INVOCATION_COMPLETED).is_empty(),
        "got INVOCATION_COMPLETED, want INVOCATION_ERROR; event types: {:?}",
        event_types(&rows)
    );
    only_row_of_type(&rows, EVENT_INVOCATION_ERROR);
}

#[tokio::test]
async fn test_runner_two_helpers_partial_then_handled_error_emits_completed() {
    let tp = setup_test_plugin().await;
    // Go streamThenErrorLLM: a partial event, then a model error.
    let helper_model = fn_llm("stream-then-error-llm", |_, _| {
        futures::stream::iter([
            partial_text("partial..."),
            Err(ModelError::Other("helper unavailable".into())),
        ])
        .boxed()
    });
    let (slow_model, slow_entered) = context_canceled_on_done_llm();
    // helper's model call starts once slow is inside its model call.
    let wait_for_slow: gadk_agent::BeforeModelCallback = {
        let slow_entered: Gate = slow_entered.clone();
        Arc::new(move |ctx: gadk_agent::Context, req| {
            let (slow_entered, token) = (slow_entered.clone(), ctx.cancellation_token().clone());
            Box::pin(async move {
                tokio::select! {
                    () = slow_entered.wait() => (req, Ok(None)),
                    () = token.cancelled() => (req, Err(AgentError::Cancelled)),
                }
            })
        })
    };
    let helper = llm_agent(LlmAgentConfig {
        name: "helper".into(),
        description: "Runs a single-turn helper task.".into(),
        mode: gadk_agent::Mode::SingleTurn,
        model: Some(helper_model),
        before_model_callbacks: vec![wait_for_slow],
        ..Default::default()
    });
    let slow = llm_agent(LlmAgentConfig {
        name: "slow".into(),
        description: "Runs a single-turn slow task.".into(),
        mode: gadk_agent::Mode::SingleTurn,
        model: Some(slow_model),
        ..Default::default()
    });
    let coordinator = llm_agent(LlmAgentConfig {
        name: "coordinator".into(),
        mode: gadk_agent::Mode::Chat,
        sub_agents: vec![helper, slow],
        model: Some(scripted_llm(vec![
            calls(vec![
                call_part("call-helper", "helper", json!({"request": "stream"})),
                call_part("call-slow", "slow", json!({"request": "wait"})),
            ]),
            final_text("done"),
        ])),
        ..Default::default()
    });
    let r = new_test_runner(coordinator, vec![tp.plugin.clone()]);
    // The consumer stops on the partial event.
    run_until(&r, "u1", "s1", "go", sse(), |item| {
        item.is_err() || is_partial_of(item, "helper")
    })
    .await;
    let rows = RowCursor::default()
        .collect(&tp, until_invocation_end)
        .await;

    // The handled helper's model error becomes its own AGENT_ERROR row.
    let counts = agent_error_counts(&rows);
    assert_eq!(
        counts.get("helper").copied().unwrap_or(0),
        1,
        "AGENT_ERROR rows for helper; event types: {:?}",
        event_types(&rows)
    );
    // The slow helper is retired quietly: no AGENT_ERROR.
    assert_eq!(
        counts.get("slow").copied().unwrap_or(0),
        0,
        "AGENT_ERROR rows for slow; event types: {:?}",
        event_types(&rows)
    );
    // The invocation completes despite the handled failure.
    assert!(
        rows_of_type(&rows, EVENT_INVOCATION_ERROR).is_empty(),
        "got INVOCATION_ERROR, want INVOCATION_COMPLETED; event types: {:?}",
        event_types(&rows)
    );
    only_row_of_type(&rows, EVENT_INVOCATION_COMPLETED);
}

#[tokio::test]
async fn test_runner_stop_on_contentless_partial_helper_emits_no_agent_error() {
    let tp = setup_test_plugin().await;
    // Go contentlessPartialLLM: one partial chunk with no content, then waits for its context.
    let contentless = fn_llm("contentless-partial-llm", |_, _| {
        then_wait_for_cancel(vec![Ok(LlmResponse {
            partial: true,
            error_code: "PROBE".into(),
            ..Default::default()
        })])
    });
    let helper = llm_agent(LlmAgentConfig {
        name: "helper".into(),
        description: "Runs a single-turn helper task.".into(),
        mode: gadk_agent::Mode::SingleTurn,
        model: Some(contentless),
        ..Default::default()
    });
    let coordinator = llm_agent(LlmAgentConfig {
        name: "coordinator".into(),
        mode: gadk_agent::Mode::Chat,
        sub_agents: vec![helper],
        model: Some(scripted_llm(vec![
            calls(vec![call_part(
                "call-helper",
                "helper",
                json!({"request": "stream"}),
            )]),
            final_text("done"),
        ])),
        ..Default::default()
    });
    let r = new_test_runner(coordinator, vec![tp.plugin.clone()]);
    run_until(&r, "u1", "s1", "go", sse(), |item| {
        item.is_err() || is_partial_of(item, "helper")
    })
    .await;
    let rows = RowCursor::default()
        .collect(&tp, until_invocation_end)
        .await;
    require_invocation_completed_ok(&rows);
    assert_eq!(
        agent_error_counts(&rows)
            .get("helper")
            .copied()
            .unwrap_or(0),
        0,
        "AGENT_ERROR rows for helper; event types: {:?}",
        event_types(&rows)
    );
}

#[tokio::test]
async fn test_runner_sequential_root_with_failed_custom_child_emits_error_terminal_rows() {
    let tp = setup_test_plugin().await;
    let child_err = "custom child failed";
    let ok_child = llm_agent(LlmAgentConfig {
        name: "ok_child".into(),
        model: Some(scripted_llm(vec![final_text("step 1 ok")])),
        ..Default::default()
    });
    let custom_fail_child = custom_agent("custom_fail_child", vec![], move |_ctx| {
        futures::stream::iter([Err(AgentError::other(child_err))]).boxed()
    });
    let seq_root = crate::trace::testkit::sequential("seq_root", vec![ok_child, custom_fail_child]);
    let r = new_test_runner(seq_root, vec![tp.plugin.clone()]);
    let (_, errs) = run_turn(&r, "u1", "s1", "go", RunConfig::default()).await;
    assert!(
        errs.len() == 1 && errs[0].to_string() == child_err,
        "r.Run errors = {errs:?}, want [{child_err}]"
    );

    let rows = RowCursor::default()
        .collect(&tp, until_invocation_end)
        .await;
    assert!(
        rows_of_type(&rows, EVENT_INVOCATION_COMPLETED).is_empty(),
        "got INVOCATION_COMPLETED rows, want 0; event types: {:?}",
        event_types(&rows)
    );
    let inv_err = only_row_of_type(&rows, EVENT_INVOCATION_ERROR);
    assert_eq!(
        str_col(&inv_err, "status"),
        "ERROR",
        "INVOCATION_ERROR status"
    );
    assert_eq!(
        agent_error_counts(&rows),
        BTreeMap::from([("custom_fail_child".to_owned(), 1)]),
        "AGENT_ERROR counts"
    );
}

// parity: adapted: Go's records carry the goroutine origins that pushAgentSpan reads from runtime.Stack; gadk's carry fiber origins (creator gadk_agent::fiber::SCHEDULER_CREATOR)
#[test]
fn test_trace_manager_record_node_completion_attributes_only_unambiguous_runs() {
    let node_path = "wf@1/x@1";
    let key = NodeRunKey {
        path: node_path.into(),
        branch: String::new(),
    };
    // workflows is the number of open activations of the workflow agent wf, each of which
    // counts `counted` unreported runs of the node x, and nodes the number of open activations
    // of x that the scheduler of the last of them started.
    struct Case {
        name: &'static str,
        workflows: u64,
        counted: i64,
        nodes: u64,
        failed: bool,
        want_ended: bool,
        want_counted: i64,
    }
    let case = |name, workflows, counted, nodes, failed, want_ended, want_counted| Case {
        name,
        workflows,
        counted,
        nodes,
        failed,
        want_ended,
        want_counted,
    };
    for tc in [
        case("one failed run", 1, 1, 1, true, true, 0),
        case("one completed run", 1, 1, 1, false, false, 1),
        case("run without an open activation", 1, 1, 0, true, false, 0),
        case("two unreported runs", 1, 2, 1, true, false, 2),
        case("two open activations", 1, 1, 2, true, false, 1),
        case("two workflow activations", 2, 1, 1, true, false, 1),
    ] {
        let tm = TraceManager::new();
        let id = ident("inv-node-completion", "wf", "", "wf@1");
        tm.invocation_span(&bg(), &id, None);
        let inv = tm.invocation(&bg(), &id);
        let (owner, nodes) = {
            let mut st = inv.mu.lock();
            let mut owner: Option<SpanRef> = None;
            for k in 0..tc.workflows {
                let parent = st.agent_stack[0].clone();
                let rec = SpanRef::new(SpanRecord {
                    span_id: new_span_id(),
                    kind: SpanKind::Agent,
                    name: "wf".into(),
                    path: "wf@1".into(),
                    parent: Some(parent),
                    is_workflow: true,
                    goroutine_id: 10 + k,
                    node_runs: [(key.clone(), tc.counted)].into(),
                    ..SpanRecord::default()
                });
                st.agent_stack.push(rec.clone());
                owner = Some(rec);
            }
            let owner = owner.expect("a workflow activation");
            let owner_goid = owner.lock().goroutine_id;
            let mut nodes = Vec::new();
            for k in 0..tc.nodes {
                let node = SpanRef::new(SpanRecord {
                    span_id: new_span_id(),
                    kind: SpanKind::Agent,
                    name: "x".into(),
                    path: node_path.into(),
                    parent: Some(owner.clone()),
                    goroutine_id: 20 + k,
                    origin: GoroutineOrigin {
                        creator: gadk_agent::fiber::SCHEDULER_CREATOR,
                        parent: owner_goid,
                    },
                    attempt: AttemptSite::Node,
                    ..SpanRecord::default()
                });
                st.agent_stack.push(node.clone());
                nodes.push(node);
            }
            (owner, nodes)
        };

        tm.record_node_completion(&bg(), &id, "wf", node_path, "", tc.failed);

        let _st = inv.mu.lock();
        for (k, node) in nodes.iter().enumerate() {
            assert_eq!(
                node.lock().node_run_ended,
                tc.want_ended,
                "{}: nodeRunEnded of activation {k} of x",
                tc.name
            );
        }
        assert_eq!(
            owner.lock().node_runs.get(&key).copied().unwrap_or(0),
            tc.want_counted,
            "{}: unreported runs of x",
            tc.name
        );
    }
}

/// Go `newInvocation(t)` of `TestTraceManager_RunsOnThisGoroutineAreOverOnlyAfterTheirFrame`.
fn runs_invocation(tm: &TraceManager) -> Arc<InvocationTrace> {
    let id = ident("inv-runs-on-this-goroutine", "custom", "", "");
    tm.invocation_span(&bg(), &id, None);
    tm.invocation(&bg(), &id)
}

/// Go `newActivation(inv, name, parent, goroutineID)`: an activation record (with an
/// activation of its own) pushed on `st`'s agent stack.
fn new_activation(
    st: &mut crate::trace::InvocationState,
    name: &str,
    parent: &SpanRef,
    goroutine_id: u64,
) -> SpanRef {
    let activation = gadk_agent::Context::builder()
        .agent(custom(name, vec![]))
        .build()
        .activation_id();
    let rec = SpanRef::new(SpanRecord {
        span_id: new_span_id(),
        kind: SpanKind::Agent,
        name: name.into(),
        parent: Some(parent.clone()),
        activation: Some(activation),
        goroutine_id,
        seq: st.next_seq_locked(),
        ..SpanRecord::default()
    });
    st.agent_stack.push(rec.clone());
    rec
}

// parity: adapted: Go's test goroutine and its `currentGoroutineID()` are a root fiber (`on_test_goroutine`) and its id (`goroutine_id`); `anotherGoroutine` is that id + 1
#[tokio::test]
async fn test_trace_manager_runs_on_this_goroutine_are_over_only_after_their_frame() {
    on_test_goroutine(async {
        // settleEndedInSubtreeLocked: since returns the since bound for child, the activation
        // that started on the test's goroutine under a custom agent that does not wait for it.
        type Since = fn(u64) -> u64;
        let cases: [(&str, Since, bool); 4] = [
            ("started after the frame", |seq| seq - 1, true),
            ("no agent run below the callback", |_| 0, true),
            ("started before the frame", |seq| seq + 1, false),
            ("no frame", |_| NO_FRAME, false),
        ];
        for (name, since, want_done) in cases {
            let goid = goroutine_id();
            assert_ne!(goid, 0, "goroutine_id() returned no ID");
            let another_goroutine = goid + 1;
            let tm = TraceManager::new();
            let inv = runs_invocation(&tm);
            let mut guard = inv.mu.lock();
            let st = &mut *guard;
            let root = st.agent_stack[0].clone();
            let custom = new_activation(st, "custom", &root, another_goroutine);
            let child = new_activation(st, "child", &custom, goid);
            let child_seq = child.lock().seq;

            inv.settle_ended_in_subtree_locked(st, 1, false, since(child_seq));

            assert_eq!(
                child.lock().run_done,
                want_done,
                "settleEndedInSubtreeLocked/{name}: runDone of an activation with seq {child_seq} on this goroutine, since {}",
                since(child_seq)
            );
            assert!(
                !custom.lock().run_done,
                "settleEndedInSubtreeLocked/{name}: runDone of the custom agent on another goroutine = true, want false"
            );
        }

        // markToolRunsDoneLocked: caller made the tool call on this goroutine, and the tool ran
        // the agent ran on it and started the agent elsewhere on another one.
        let goid = goroutine_id();
        assert_ne!(goid, 0, "goroutine_id() returned no ID");
        let another_goroutine = goid + 1;
        let tm = TraceManager::new();
        let inv = runs_invocation(&tm);
        let mut guard = inv.mu.lock();
        let st = &mut *guard;
        let root = st.agent_stack[0].clone();
        let caller = new_activation(st, "caller", &root, goid);
        let call = SpanRef::new(SpanRecord {
            span_id: new_span_id(),
            kind: SpanKind::Tool,
            parent: Some(caller.clone()),
            seq: st.next_seq_locked(),
            ..SpanRecord::default()
        });
        let ran = new_activation(st, "ran", &caller, goid);
        let elsewhere = new_activation(st, "elsewhere", &caller, another_goroutine);

        inv.mark_tool_runs_done_locked(st, &call);

        for (rec, want) in [(&caller, false), (&ran, true), (&elsewhere, false)] {
            let r = rec.lock();
            assert_eq!(r.run_done, want, "markToolRunsDoneLocked: runDone of {}", r.name);
        }
    })
    .await;
}

#[tokio::test]
async fn test_runner_parallel_tool_runs_of_silently_failing_agents_emit_agent_errors() {
    let tp = setup_test_plugin().await;
    let new_run_tool = |name: &str| -> Arc<dyn Tool> {
        let failing = custom_agent(name, vec![], {
            let msg = format!("{name} failed");
            move |_ctx| futures::stream::iter([Err(AgentError::other(msg.clone()))]).boxed()
        });
        let sub_path = format!("{name}@1");
        function_tool(
            &format!("run_{name}"),
            &format!("Runs {name}"),
            move |ctx: gadk_agent::Context, _args| {
                let (failing, sub_path) = (failing.clone(), sub_path.clone());
                async move {
                    let promoted = ctx.with_delta(ContextDelta {
                        path: Some(sub_path),
                        ..ContextDelta::default()
                    });
                    let mut events = failing.run(promoted.as_invocation().clone());
                    while let Some(item) = events.next().await {
                        if let Err(e) = item {
                            return Err(gadk_agent::ToolError::Execution(Box::new(e)));
                        }
                    }
                    Ok(json!({"ok": true}).as_object().cloned().unwrap_or_default())
                }
            },
        )
    };
    let coordinator = llm_agent(LlmAgentConfig {
        name: "coordinator".into(),
        tools: vec![new_run_tool("failing_a"), new_run_tool("failing_b")],
        model: Some(scripted_llm(vec![
            calls(vec![
                call_part("call-a", "run_failing_a", json!({})),
                call_part("call-b", "run_failing_b", json!({})),
            ]),
            final_text("both helpers failed"),
        ])),
        ..Default::default()
    });
    let r = new_test_runner(coordinator, vec![tp.plugin.clone()]);
    let (_, errs) = run_turn(&r, "u1", "s1", "go", RunConfig::default()).await;
    assert!(errs.is_empty(), "runner.Run error: {errs:?}");
    let rows = RowCursor::default()
        .collect(&tp, until_invocation_end)
        .await;
    assert!(
        rows_of_type(&rows, EVENT_INVOCATION_ERROR).is_empty(),
        "got INVOCATION_ERROR, want INVOCATION_COMPLETED; event types: {:?}",
        event_types(&rows)
    );
    only_row_of_type(&rows, EVENT_INVOCATION_COMPLETED);
    assert_eq!(
        agent_error_counts(&rows),
        BTreeMap::from([("failing_a".to_owned(), 1), ("failing_b".to_owned(), 1)]),
        "AGENT_ERROR rows per agent"
    );
}

// parity: adapted: gadk runs the after-run callbacks of a stream dropped early on a task of its own (Go: on the consumer's goroutine before `break` returns), so the test waits for them before it reads the completed runs
#[tokio::test]
async fn test_runner_stopped_streams_keep_completed_runs_bounded() {
    let mut tm = TraceManager::new();
    tm.max_tracked = 1;
    let limit = tm.max_tracked.max(64);
    let tm = Arc::new(tm);
    let p = new_trace_only_plugin(tm.clone());
    let bot = llm_agent(LlmAgentConfig {
        name: "bot".into(),
        model: Some(super::attr_support::partial_then_wait_llm("hello ")),
        ..Default::default()
    });
    let r = new_test_runner(bot, vec![p]);

    for i in 0..limit + 6 {
        let mut saw_partial = false;
        let (_, errs) = run_until(&r, "u1", &format!("s{i}"), "hi", sse(), |item| {
            saw_partial = matches!(item, Ok(ev) if ev.response.partial);
            saw_partial || item.is_err()
        })
        .await;
        assert!(errs.is_empty(), "run {i}: r.Run error: {errs:?}");
        assert!(saw_partial, "run {i} yielded no partial event");
        wait_for_after_runs(&tm).await;
    }

    let st = tm.mu.lock();
    assert!(
        st.completed.len() <= limit,
        "completed runs = {}, want at most {limit}",
        st.completed.len()
    );
    for (id, c) in &st.completed {
        assert!(
            c.late.is_none(),
            "completed run {id:?} kept late state for a stream that its caller stopped reading"
        );
    }
}

#[tokio::test]
async fn test_runner_failed_model_runs_keep_no_late_state() {
    let tm = Arc::new(TraceManager::new());
    let p = new_trace_only_plugin(tm.clone());
    let bot = llm_agent(LlmAgentConfig {
        name: "bot".into(),
        model: Some(failing_llm("model unavailable")),
        ..Default::default()
    });
    let r = new_test_runner(bot, vec![p]);
    let runs = 20;
    for i in 0..runs {
        let (_, errs) = run_turn(&r, "u1", &format!("s{i}"), "hi", RunConfig::default()).await;
        assert!(!errs.is_empty(), "run {i}: r.Run yielded no error");
    }

    let st = tm.mu.lock();
    assert_eq!(st.completed.len(), runs, "completed runs");
    for (id, c) in &st.completed {
        assert!(c.late.is_none(), "completed run {id:?} keeps late state");
    }
}

#[tokio::test]
async fn test_repro_failed_model_calls_pin_completed_cache() {
    let mut tm = TraceManager::new();
    tm.max_tracked = 1;
    let limit = tm.max_tracked.max(64);
    let tm = Arc::new(tm);
    let mut config = default_config();
    config.disable_flush_on_run_end = true;
    let ap = Arc::new(AnalyticsPlugin {
        config,
        allowlist: ["NO_SUCH_EVENT".to_owned()].into(),
        traces: Some(tm.clone()),
        ..AnalyticsPlugin::default()
    });
    // Go's "trace-bound" plugin registers no tool callbacks.
    let p = gadk_agent::Plugin::new(gadk_agent::PluginConfig {
        before_tool_callback: None,
        after_tool_callback: None,
        on_tool_error_callback: None,
        ..callbacks_config("trace-bound", &ap)
    });
    let guarded = llm_agent(LlmAgentConfig {
        name: "guarded".into(),
        model: Some(scripted_llm(vec![Ok(LlmResponse {
            content: Some(text_content("model", "never")),
            ..Default::default()
        })])),
        before_model_callbacks: vec![Arc::new(|_ctx, req| {
            Box::pin(async move { (req, Err(AgentError::other("blocked by guardrail"))) })
        })],
        ..Default::default()
    });
    let rg = new_test_runner(guarded, vec![p.clone()]);
    for i in 0..limit + 6 {
        run_turn(&rg, "u1", &format!("g{i}"), "hi", RunConfig::default()).await;
    }
    let pinned = tm
        .mu
        .lock()
        .completed
        .values()
        .filter(|c| c.late.as_ref().is_some_and(|l| l.has_open_calls()))
        .count();

    let bot = llm_agent(LlmAgentConfig {
        name: "bot".into(),
        model: Some(scripted_llm(vec![final_text("ok")])),
        ..Default::default()
    });
    let (events, errs) = run_turn(
        &new_test_runner(bot, vec![p]),
        "u1",
        "ordinary",
        "hi",
        RunConfig::default(),
    )
    .await;
    assert!(errs.is_empty(), "{errs:?}");
    let inv_id = events
        .first()
        .map(|ev| ev.invocation_id.clone())
        .unwrap_or_default();
    let kept = tm.mu.lock().completed.contains_key(&inv_id);
    assert!(
        pinned == 0 && kept,
        "runs pinned by a call with no callback = {pinned}, want 0; ordinary run's identity kept = {kept}, want true"
    );
}

// parity: adapted: Go's endedContext is an ADK context whose invocation ended (`ended_ctx`), and the test goroutine on which the shortcut starts and the tool call returns is a root fiber
#[tokio::test]
async fn test_late_review_shortcut_under_invocation_span() {
    on_test_goroutine(async {
        let tm = TraceManager::new();
        let inv = "inv-root-shortcut";
        let live = ident(inv, "live_agent", "", "");
        let call = tool_key(&live, "call-1", "t");
        tm.invocation_span(&bg(), &live, None);
        tm.push_agent_span(&bg(), &live);
        tm.push_tool_span(&bg(), &live, &call);
        tm.pop_agent_span(&bg(), &live);
        tm.end_invocation(&bg(), &live);
        let sub = ident(inv, "sub", "", "");
        tm.push_agent_span(&ended_ctx(), &sub);
        let sub_rec = late_review_agent(&tm, inv, "sub");
        tm.pop_tool_span(&bg(), &live, &call, false);

        let late = late_review_trace(&tm, inv);
        let got = late_review_stack(&tm, inv);
        assert!(
            got.is_empty(),
            "late agents after the tool call returned = {got:?}, want none"
        );
        assert!(
            !late.open_agents.load(std::sync::atomic::Ordering::SeqCst),
            "late.openAgents = true after the tool call returned, want false"
        );
        check_late_record_dropped(&late, &sub_rec);
    })
    .await;
}
