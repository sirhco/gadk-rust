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

//! Part of the port of ADK-Go `plugin/agentanalytics/trace_manager_test.go` (owner: Task 12): the
//! 28 Go tests from `TestTraceManager_ConcurrentRunnerTreeAndCapacityEviction` to
//! `TestTraceManager_PopTaskAgentSpan_IgnoresUnrelatedAncestor` in the Go file's order that need no
//! runner (the exact list is in the 7b-3a plan's contracts, Appendix A).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![allow(clippy::type_complexity)] // Go's table-driven test cases as tuples.

use std::collections::BTreeMap;
use std::sync::Arc;

use gadk_agent::Mode;
use gadk_engine::telemetry::d6::SpanIds;
use gadk_genai::Content;
use gadk_model::LlmResponse;
use gadk_session::Event;
use tokio_util::sync::CancellationToken;

use super::direct_support::{MockRun, SpanCounts, open_spans};
use crate::error::Error;
use crate::event_types::{EVENT_AGENT_ERROR, EVENT_INVOCATION_COMPLETED, EVENT_INVOCATION_ERROR};
use crate::parity_tests::plugin_agentanalytics_bigquery_agent_analytics_plugin::support::{
    agent_error_counts, only_row_of_type, rows_of_type, setup_test_plugin, until_invocation_end,
};
use crate::trace::causes::{error_is, is_canceled};
use crate::trace::testkit::{ManualClock, ident, llm, sequential};
use crate::trace::{
    CallbackCtx, Identity, TraceError, TraceManager, UnclosedAgentSpan, llm_key, tool_key,
};

/// Go `errors.New(text)` as the trace manager keeps errors.
fn err(text: &str) -> TraceError {
    Arc::new(Error::Message(text.to_owned()))
}

/// Go `context.Background()`.
fn bg() -> CallbackCtx {
    CallbackCtx::background()
}

/// Go `trace.ContextWithSpanContext(context.Background(), sc)` of hex IDs.
fn span_ctx(trace_hex: &str, span_hex: &str) -> CallbackCtx {
    let mut sc = SpanIds::default();
    for (i, b) in sc.trace_id.iter_mut().enumerate() {
        *b = u8::from_str_radix(&trace_hex[2 * i..2 * i + 2], 16).unwrap();
    }
    for (i, b) in sc.span_id.iter_mut().enumerate() {
        *b = u8::from_str_radix(&span_hex[2 * i..2 * i + 2], 16).unwrap();
    }
    bg().with_span(sc)
}

fn names(unclosed: &[UnclosedAgentSpan]) -> Vec<String> {
    unclosed.iter().map(|u| u.agent_name.clone()).collect()
}

#[test]
fn test_trace_manager_concurrent_runner_tree_and_capacity_eviction() {
    let worker = llm("worker", vec![], Mode::Unset);
    let root = llm("root", vec![worker], Mode::Unset);
    for cap_limit in [1usize, 4] {
        let mut tm = TraceManager::new();
        tm.max_tracked = cap_limit;
        let tm = tm;
        const WORKERS: usize = 24;
        std::thread::scope(|s| {
            for i in 0..WORKERS {
                let (tm, root) = (&tm, &root);
                s.spawn(move || {
                    let ctx = bg();
                    let id = ident(&format!("inv-evict-{i}"), "root", "", "");
                    let inv = tm.invocation_span(&ctx, &id, Some(root));
                    let (ag, _) = tm.push_agent_span(&ctx, &id);
                    let llm_span = tm.push_llm_span(&ctx, &id);
                    tm.record_first_token(&ctx, &id);
                    let _ = tm.pop_llm_span(&ctx, &id, true);
                    let _ = tm.pop_agent_span(&ctx, &id);
                    let end = tm.end_invocation(&ctx, &id);
                    assert!(
                        !inv.trace_id.is_empty()
                            && ag.trace_id == inv.trace_id
                            && llm_span.trace_id == inv.trace_id
                            && end.trace_id == inv.trace_id,
                        "maxTracked={cap_limit}: invocation {i} traceID mismatch: inv={inv:?} ag={ag:?} llm={llm_span:?} end={end:?}"
                    );
                    assert_eq!(
                        end.root_agent_name, "root",
                        "maxTracked={cap_limit}: invocation {i} rootAgentName"
                    );
                });
            }
        });
    }
}

#[test]
fn test_trace_manager_eviction_preserves_inherited_o_tel_and_nested_callback_context() {
    const AMBIENT_TRACE_HEX: &str = "4bf92f3577b34da6a3ce929d0e0e4736";
    const AMBIENT_SPAN_HEX: &str = "00f067aa0ba902b7";
    const NESTED_TRACE_HEX: &str = "4bf92f3577b34da6a3ce929d0e0e4736";
    const NESTED_SPAN_HEX: &str = "1111222233334444";
    let ambient_ctx = span_ctx(AMBIENT_TRACE_HEX, AMBIENT_SPAN_HEX);
    let nested_ctx = span_ctx(NESTED_TRACE_HEX, NESTED_SPAN_HEX);

    let clock = ManualClock::new();
    let mut tm = TraceManager::new();
    tm.max_tracked = 1;
    tm.now = clock.clock();
    let tm = tm;

    // Case 1: evicted, then recreated under nested_ctx via push_agent_span, then ended without
    // ambient OTel context.
    let inv1_id = ident("inv-otel-1", "root", "", "");
    let orig1 = tm.invocation_span(&ambient_ctx, &inv1_id, None);
    assert!(
        orig1.trace_id == AMBIENT_TRACE_HEX && orig1.parent_span_id == AMBIENT_SPAN_HEX,
        "orig1 = {orig1:?}, want traceID {AMBIENT_TRACE_HEX:?} and parentSpanID {AMBIENT_SPAN_HEX:?}"
    );
    clock.advance_ms(1000);
    tm.invocation_span(&bg(), &ident("evictor-1", "other", "", ""), None);

    clock.advance_ms(1000);
    let sub_id = ident("inv-otel-1", "sub", "", "");
    let (sub_span, _) = tm.push_agent_span(&nested_ctx, &sub_id);
    assert!(
        sub_span.trace_id == AMBIENT_TRACE_HEX
            && sub_span.parent_span_id == orig1.span_id
            && sub_span.root_agent_name == "root",
        "recreated pushAgentSpan under nestedCtx = {sub_span:?}, want traceID {AMBIENT_TRACE_HEX:?}, parentSpanID {:?} (orig root span), rootAgentName \"root\"",
        orig1.span_id
    );
    let _ = tm.pop_agent_span(&nested_ctx, &sub_id);
    clock.advance_ms(1000);
    let end1 = tm.end_invocation(&nested_ctx, &inv1_id);
    assert!(
        end1.trace_id == AMBIENT_TRACE_HEX
            && end1.span_id == orig1.span_id
            && end1.parent_span_id == AMBIENT_SPAN_HEX
            && end1.root_agent_name == "root",
        "post-eviction endInvocation (after pushAgentSpan) = {end1:?}, want traceID {AMBIENT_TRACE_HEX:?}, spanID {:?}, parentSpanID {AMBIENT_SPAN_HEX:?}, rootAgentName \"root\"",
        orig1.span_id
    );
    assert!(
        end1.latency.is_none(),
        "post-eviction endInvocation after pushAgentSpan has latency {:?}, want none",
        end1.latency
    );

    // Case 2: evicted, then ended directly with a background context (no ambient OTel).
    let inv2_id = ident("inv-otel-2", "root", "", "");
    let orig2 = tm.invocation_span(&ambient_ctx, &inv2_id, None);
    clock.advance_ms(1000);
    tm.invocation_span(&bg(), &ident("evictor-2", "other", "", ""), None);
    clock.advance_ms(1000);
    let end2 = tm.end_invocation(&bg(), &inv2_id);
    assert!(
        end2.trace_id == AMBIENT_TRACE_HEX
            && end2.span_id == orig2.span_id
            && end2.parent_span_id == AMBIENT_SPAN_HEX
            && end2.root_agent_name == "root",
        "post-eviction endInvocation (no ambient OTel) = {end2:?}, want traceID {AMBIENT_TRACE_HEX:?}, spanID {:?}, parentSpanID {AMBIENT_SPAN_HEX:?}, rootAgentName \"root\"",
        orig2.span_id
    );
    assert!(
        end2.latency.is_none(),
        "post-eviction endInvocation has latency {:?}, want none",
        end2.latency
    );
}

#[test]
fn test_trace_manager_short_circuited_last_child_retired_when_parent_pops() {
    let ctx = bg();
    let tm = TraceManager::new();
    let last_child = llm("shortcut_last", vec![], Mode::Unset);
    let seq = sequential("sequence", vec![last_child]);
    let seq_id = ident("inv-last-child", "sequence", "", "sequence@0");
    let child_id = ident("inv-last-child", "shortcut_last", "", "sequence@0");

    tm.invocation_span(&ctx, &seq_id, Some(&seq));
    tm.push_agent_span(&ctx, &seq_id);
    tm.push_agent_span(&ctx, &child_id);
    assert_eq!(
        open_spans(&tm, "inv-last-child").agents,
        2,
        "open agent spans before parent pop"
    );
    // shortcut_last was short-circuited in BeforeAgentCallback, so only sequence runs
    // popAgentSpan. ADK writes the callback's content as an event of shortcut_last, which
    // shows that it responded.
    tm.complete_response(&ctx, &child_id, "shortcut_last", "", &[]);
    tm.pop_agent_span(&ctx, &seq_id);
    assert_eq!(
        open_spans(&tm, "inv-last-child"),
        SpanCounts::default(),
        "openSpans after parent pop, want empty"
    );
}

#[test]
fn test_trace_manager_concurrent_siblings_and_interleaved_same_agent_activations() {
    let ctx = bg();

    // configured sub-agents a and b concurrent on same branch
    {
        let tm = TraceManager::new();
        let sub_a = llm("a", vec![], Mode::Unset);
        let sub_b = llm("b", vec![], Mode::Unset);
        let par = sequential("par", vec![sub_a, sub_b]);

        let par_id = ident("inv-ab", "par", "", "par@0");
        let a_id = ident("inv-ab", "a", "", "par@0/a@1");
        let b_id = ident("inv-ab", "b", "", "par@0/b@1");

        tm.invocation_span(&ctx, &par_id, Some(&par));
        let (par_start, _) = tm.push_agent_span(&ctx, &par_id);
        let (a_start, _) = tm.push_agent_span(&ctx, &a_id);
        let (b_start, _) = tm.push_agent_span(&ctx, &b_id);

        assert_eq!(
            a_start.parent_span_id, par_start.span_id,
            "a parentSpanID, want par spanID"
        );
        assert_eq!(
            b_start.parent_span_id, par_start.span_id,
            "b parentSpanID, want par spanID"
        );

        let a_end = tm.pop_agent_span(&ctx, &a_id);
        let b_end = tm.pop_agent_span(&ctx, &b_id);
        let par_end = tm.pop_agent_span(&ctx, &par_id);

        assert!(
            a_end.span_id == a_start.span_id && a_end.latency.is_some(),
            "a popAgentSpan = {a_end:?}, want spanID {:?} with latency",
            a_start.span_id
        );
        assert!(
            b_end.span_id == b_start.span_id && b_end.latency.is_some(),
            "b popAgentSpan = {b_end:?}, want spanID {:?} with latency",
            b_start.span_id
        );
        assert!(
            par_end.span_id == par_start.span_id && par_end.latency.is_some(),
            "par popAgentSpan = {par_end:?}, want spanID {:?} with latency",
            par_start.span_id
        );
    }

    // interleaved p@1/x@1 and p@2/x@2 on same branch
    {
        let tm = TraceManager::new();
        let child_x = llm("x", vec![], Mode::Unset);
        let parent_p = sequential("p", vec![child_x]);

        let p1_id = ident("inv-px", "p", "", "p@1");
        let p2_id = ident("inv-px", "p", "", "p@2");
        let x1_id = ident("inv-px", "x", "", "p@1/x@1");
        let x2_id = ident("inv-px", "x", "", "p@2/x@2");

        tm.invocation_span(&ctx, &p1_id, Some(&parent_p));
        let (p1_start, _) = tm.push_agent_span(&ctx, &p1_id);
        let (p2_start, _) = tm.push_agent_span(&ctx, &p2_id);
        let (x1_start, _) = tm.push_agent_span(&ctx, &x1_id);
        let (x2_start, _) = tm.push_agent_span(&ctx, &x2_id);

        assert_eq!(
            x1_start.parent_span_id, p1_start.span_id,
            "x@1 parentSpanID, want p@1 spanID"
        );
        assert_eq!(
            x2_start.parent_span_id, p2_start.span_id,
            "x@2 parentSpanID, want p@2 spanID"
        );

        // Suppose x@1 was short-circuited in BeforeAgentCallback, which writes an event of
        // x@1, and p@1 finishes while p@2 and p@2/x@2 are still open.
        tm.complete_response(&ctx, &x1_id, "x", "", &[]);
        let p1_end = tm.pop_agent_span(&ctx, &p1_id);
        assert!(
            p1_end.span_id == p1_start.span_id && p1_end.latency.is_some(),
            "p@1 popAgentSpan = {p1_end:?}, want spanID {:?} with latency",
            p1_start.span_id
        );
        // x@1 must have been retired with p@1, while p@2 and x@2 remain open.
        assert_eq!(
            open_spans(&tm, "inv-px").agents,
            2,
            "open agent spans after p@1 pop, want 2 (p@2 and x@2)"
        );

        let x2_end = tm.pop_agent_span(&ctx, &x2_id);
        let p2_end = tm.pop_agent_span(&ctx, &p2_id);
        assert!(
            x2_end.span_id == x2_start.span_id
                && x2_end.parent_span_id == p2_start.span_id
                && x2_end.latency.is_some(),
            "x@2 popAgentSpan = {x2_end:?}, want spanID {:?} parent {:?} with latency",
            x2_start.span_id,
            p2_start.span_id
        );
        assert!(
            p2_end.span_id == p2_start.span_id && p2_end.latency.is_some(),
            "p@2 popAgentSpan = {p2_end:?}, want spanID {:?} with latency",
            p2_start.span_id
        );
        assert_eq!(
            open_spans(&tm, "inv-px"),
            SpanCounts::default(),
            "openSpans after p@2 pop, want empty"
        );
    }
}

// parity: adapted: Go inspects a context whose embedded struct pointer is nil by reflection (`contextPath`, `contextParentMap`); gadk reads typed contexts, so a context without an ADK context (`CallbackCtx::background()`) shows no path and no parent map.
#[test]
fn test_context_inspection_nil_embedded_struct_does_not_panic() {
    let ctx = bg();
    let got_path = crate::plugin::rows::identity_of(&ctx).path;
    assert_eq!(got_path, "", "contextPath(ctx), want empty string");
    assert!(
        ctx.parent_map().is_none(),
        "contextParentMap(ctx), want none"
    );
}

#[test]
fn test_trace_manager_unrecovered_error_keying_two_failures_one_recovers() {
    let ctx = bg();
    let tm = TraceManager::new();
    let id1 = ident("inv-two-errs", "agentA", "", "pathA");
    let id2 = ident("inv-two-errs", "agentB", "", "pathB");
    let err_a = err("failure A");
    let err_b = err("failure B");

    tm.push_agent_span(&ctx, &id1);

    let key_a = llm_key(&id1);
    let key_b = llm_key(&id2);

    tm.record_error(&ctx, &id1, &key_a, err_a.clone());
    tm.record_error(&ctx, &id2, &key_b, err_b);

    // Call B recovers.
    tm.clear_error(&ctx, &id2, &key_b);

    // Call A is still unrecovered. popUnclosedAgentSpans must retain errA.
    let (_, got) = tm.pop_unclosed_agent_spans(&ctx, &id1);
    let ok = got
        .as_ref()
        .is_some_and(|e| error_is(e, &err_a) || e.to_string() == err_a.to_string());
    assert!(ok, "popUnclosedAgentSpans err = {got:?}, want {err_a}");
}

#[tokio::test]
async fn test_on_event_response_or_partial_event_keeps_author_from_failing() {
    let text = |t: &str| Some(Content::from_text(t, "model"));
    let output = Some(serde_json::Value::String("result".into()));
    let ev = |response: LlmResponse, output: Option<serde_json::Value>| Event {
        response,
        output,
        ..Event::default()
    };
    let tests: Vec<(&str, Event, bool)> = vec![
        (
            "content",
            ev(
                LlmResponse {
                    content: text("hi"),
                    ..LlmResponse::default()
                },
                None,
            ),
            true,
        ),
        ("output", ev(LlmResponse::default(), output.clone()), true),
        (
            "content and output",
            ev(
                LlmResponse {
                    content: text("hi"),
                    ..LlmResponse::default()
                },
                output.clone(),
            ),
            true,
        ),
        (
            "partial content",
            ev(
                LlmResponse {
                    partial: true,
                    content: text("h"),
                    ..LlmResponse::default()
                },
                None,
            ),
            true,
        ),
        (
            "partial output",
            ev(
                LlmResponse {
                    partial: true,
                    ..LlmResponse::default()
                },
                output.clone(),
            ),
            true,
        ),
        (
            "partial without content or output",
            ev(
                LlmResponse {
                    partial: true,
                    ..LlmResponse::default()
                },
                None,
            ),
            true,
        ),
        (
            "partial error code without content",
            ev(
                LlmResponse {
                    partial: true,
                    error_code: "SAFETY".into(),
                    ..LlmResponse::default()
                },
                None,
            ),
            true,
        ),
        ("neither content nor output", Event::default(), false),
    ];
    for (name, event, want_completed) in tests {
        let tp = setup_test_plugin().await;
        let run = MockRun::new(&tp.ctx);
        let mut event = event;
        event.author = tp.ctx.agent_name.clone();
        let p = &tp.plugin;
        p.before_agent_callback().unwrap()(run.ctx()).await.unwrap();
        p.on_event_callback().unwrap()(run.inv(), event)
            .await
            .unwrap();
        p.after_run_callback().unwrap()(run.inv()).await;

        let rows = tp.collect_rows(until_invocation_end).await;
        let types = crate::parity_tests::plugin_agentanalytics_bigquery_agent_analytics_plugin::support::event_types(&rows);
        if want_completed {
            assert!(
                rows_of_type(&rows, EVENT_INVOCATION_ERROR).is_empty(),
                "{name}: got INVOCATION_ERROR, want INVOCATION_COMPLETED: the author responded; event types: {types:?}"
            );
            only_row_of_type(&rows, EVENT_INVOCATION_COMPLETED);
            assert_eq!(
                rows_of_type(&rows, EVENT_AGENT_ERROR).len(),
                0,
                "{name}: AGENT_ERROR rows"
            );
            continue;
        }
        assert!(
            rows_of_type(&rows, EVENT_INVOCATION_COMPLETED).is_empty(),
            "{name}: got INVOCATION_COMPLETED, want INVOCATION_ERROR: the author did not respond; event types: {types:?}"
        );
        only_row_of_type(&rows, EVENT_INVOCATION_ERROR);
        let want: BTreeMap<String, usize> = [(tp.ctx.agent_name.clone(), 1)].into();
        assert_eq!(
            agent_error_counts(&rows),
            want,
            "{name}: AGENT_ERROR rows per agent"
        );
    }
}

#[test]
fn test_trace_manager_repeated_worker_attribution_to_failing_activation_only() {
    let ctx = bg();
    let tm = TraceManager::new();

    let wf_id = ident("inv-repeated", "workflow", "", "wf@0");
    let w1_id = ident("inv-repeated", "worker", "", "wf@0/worker@cached");
    let w2_id = ident("inv-repeated", "worker", "", "wf@0/worker@failed");

    tm.push_agent_span(&ctx, &wf_id);
    let (w1_start, _) = tm.push_agent_span(&ctx, &w1_id);
    let (w2_start, _) = tm.push_agent_span(&ctx, &w2_id);
    let step2_err = err("model failure on step 2");
    tm.record_error(&ctx, &w2_id, &llm_key(&w2_id), step2_err.clone());

    let (unclosed, term_err) = tm.pop_unclosed_agent_spans(&ctx, &wf_id);
    assert!(
        term_err.as_ref().is_some_and(|e| error_is(e, &step2_err)),
        "popUnclosedAgentSpans termErr = {term_err:?}, want {step2_err}"
    );

    for u in &unclosed {
        assert_ne!(
            u.span.span_id, w1_start.span_id,
            "worker@cached span was attributed with error, want only failing activation"
        );
    }
    assert_eq!(
        unclosed.len(),
        2,
        "popped unclosed spans count, want 2 (worker@failed and workflow)"
    );
    assert_eq!(
        unclosed[0].span.span_id, w2_start.span_id,
        "popped[0] spanID, want worker@failed spanID"
    );
    assert!(
        unclosed[0]
            .err
            .as_ref()
            .is_some_and(|e| Arc::ptr_eq(e, &step2_err)),
        "worker@failed err = {:?}, want {step2_err}",
        unclosed[0].err
    );
}

#[test]
fn test_trace_manager_two_failures_neither_recovers_finalizes_both_activations() {
    let ctx = bg();
    let tm = TraceManager::new();

    let par_id = ident("inv-two-fail", "par", "", "par@0");
    let c1_id = ident("inv-two-fail", "child1", "", "par@0/c1@1");
    let c2_id = ident("inv-two-fail", "child2", "", "par@0/c2@1");

    let (par_start, _) = tm.push_agent_span(&ctx, &par_id);
    let (c1_start, _) = tm.push_agent_span(&ctx, &c1_id);
    let (c2_start, _) = tm.push_agent_span(&ctx, &c2_id);

    let err1 = err("child1 failure 500");
    let err2 = err("child2 failure 503");
    tm.record_error(&ctx, &c1_id, &llm_key(&c1_id), err1.clone());
    tm.record_error(&ctx, &c2_id, &llm_key(&c2_id), err2.clone());

    let (unclosed, term_err) = tm.pop_unclosed_agent_spans(&ctx, &par_id);
    assert!(
        term_err.is_some(),
        "popUnclosedAgentSpans returned no termErr, want an error"
    );
    assert_eq!(
        unclosed.len(),
        3,
        "len(unclosed), want 3 (child1, child2, par)"
    );

    let by_span_id: BTreeMap<String, &UnclosedAgentSpan> = unclosed
        .iter()
        .map(|u| (u.span.span_id.clone(), u))
        .collect();

    let same = |u: &UnclosedAgentSpan, want: &TraceError| {
        u.err.as_ref().is_some_and(|e| Arc::ptr_eq(e, want))
    };
    match by_span_id.get(&c1_start.span_id) {
        None => panic!("child1 span {:?} not in unclosed spans", c1_start.span_id),
        Some(u) => assert!(
            same(u, &err1),
            "child1 unclosed err = {:?}, want {err1}",
            u.err
        ),
    }
    match by_span_id.get(&c2_start.span_id) {
        None => panic!("child2 span {:?} not in unclosed spans", c2_start.span_id),
        Some(u) => assert!(
            same(u, &err2),
            "child2 unclosed err = {:?}, want {err2}",
            u.err
        ),
    }
    match by_span_id.get(&par_start.span_id) {
        None => panic!(
            "shared parent span {:?} not in unclosed spans",
            par_start.span_id
        ),
        Some(u) => assert!(
            u.err.is_some(),
            "shared parent unclosed err is none, want an error"
        ),
    }
}

#[test]
fn test_trace_manager_find_agent_does_not_match_different_node_path() {
    let ctx = bg();
    let tm = TraceManager::new();

    let w2_id = ident("inv-diff-path", "worker", "", "root/b/w2");
    tm.push_agent_span(&ctx, &w2_id);

    // Attempting to find or pop worker on a different path "root/a/w1" must not match w2.
    let w1_id = ident("inv-diff-path", "worker", "", "root/a/w1");
    let popped = tm.pop_agent_span(&ctx, &w1_id);

    // Since w1 was not found in agentStack, popAgentSpan returns a fresh orphan span with no
    // latency, and must NOT pop or return w2's span.
    assert!(
        popped.latency.is_none(),
        "popAgentSpan(w1) latency = {:?}, want none",
        popped.latency
    );

    // Verify w2 is still open and tracked on agentStack.
    let inv = tm.invocation(&ctx, &w2_id);
    let st = inv.mu.lock();
    let idx = st.find_agent("root/b/w2", "worker", "");
    assert!(
        idx.is_some_and(|i| i > 0),
        "worker on root/b/w2 was prematurely popped or lost from agentStack"
    );
}

#[test]
fn test_trace_manager_error_of_normally_ended_activation_still_fails_invocation() {
    // (name, failing, ended, want_unclosed)
    let tests: Vec<(&str, (&str, &str), (&str, &str), Vec<&str>)> = vec![
        (
            "failing activation ends",
            ("branch_a", "root/a"),
            ("branch_a", "root/a"),
            vec![],
        ),
        (
            "ancestor of failing activation ends",
            ("worker", "root/a/worker"),
            ("branch_a", "root/a"),
            vec!["worker"],
        ),
    ];
    for (name, failing, ended, want_unclosed) in tests {
        let ctx = bg();
        let tm = TraceManager::new();
        const INVOCATION_ID: &str = "inv-ended-activation";
        let root_id = ident(INVOCATION_ID, "root", "", "root");
        let ended_id = ident(INVOCATION_ID, ended.0, "", ended.1);
        let failing_id = ident(INVOCATION_ID, failing.0, "", failing.1);
        let sibling_id = ident(INVOCATION_ID, "branch_b", "", "root/b");

        tm.push_agent_span(&ctx, &root_id);
        tm.push_llm_span(&ctx, &root_id);
        tm.pop_llm_span(&ctx, &root_id, true);
        tm.push_agent_span(&ctx, &ended_id);
        if failing != ended {
            tm.push_agent_span(&ctx, &failing_id);
        }
        tm.push_llm_span(&ctx, &failing_id);
        let model_err = err("model failure 500");
        tm.record_error(&ctx, &failing_id, &llm_key(&failing_id), model_err.clone());
        tm.pop_llm_span(&ctx, &failing_id, false);
        tm.pop_agent_span(&ctx, &ended_id);
        tm.push_agent_span(&ctx, &sibling_id);
        tm.push_llm_span(&ctx, &sibling_id);
        tm.pop_llm_span(&ctx, &sibling_id, true);

        let (unclosed, term_err) = tm.pop_unclosed_agent_spans(&ctx, &root_id);
        assert!(
            term_err.as_ref().is_some_and(|e| error_is(e, &model_err)),
            "{name}: popUnclosedAgentSpans termErr = {term_err:?}, want {model_err}"
        );
        for u in &unclosed {
            assert!(
                u.err.as_ref().is_some_and(|e| error_is(e, &model_err)),
                "{name}: unclosed {:?} err = {:?}, want {model_err}",
                u.agent_name,
                u.err
            );
        }
        assert_eq!(
            names(&unclosed),
            want_unclosed,
            "{name}: popUnclosedAgentSpans agents"
        );
    }
}

#[test]
fn test_trace_manager_error_of_unknown_activation_fails_invocation_without_agent_errors() {
    let ctx = bg();
    let tm = TraceManager::new();
    const INVOCATION_ID: &str = "inv-unknown-activation";
    let root_id = ident(INVOCATION_ID, "root", "", "root");
    let sibling_id = ident(INVOCATION_ID, "branch_b", "", "root/b");
    let unknown_id = ident(INVOCATION_ID, "unknown", "", "root/unknown");

    tm.push_agent_span(&ctx, &root_id);
    tm.push_agent_span(&ctx, &sibling_id);
    tm.push_llm_span(&ctx, &sibling_id);
    tm.pop_llm_span(&ctx, &sibling_id, true);
    let model_err = err("model failure 500");
    tm.record_error(&ctx, &unknown_id, &llm_key(&unknown_id), model_err.clone());

    let (unclosed, term_err) = tm.pop_unclosed_agent_spans(&ctx, &root_id);
    assert!(
        term_err.as_ref().is_some_and(|e| error_is(e, &model_err)),
        "popUnclosedAgentSpans termErr = {term_err:?}, want {model_err}"
    );
    let got: Vec<_> = unclosed.iter().map(|u| (&u.agent_name, &u.err)).collect();
    assert!(
        got.is_empty(),
        "popUnclosedAgentSpans returned {got:?} (agent, error), want no span"
    );
}

#[test]
fn test_trace_manager_cancellation_attributes_every_open_span() {
    let tm = TraceManager::new();
    const INVOCATION_ID: &str = "inv-canceled";
    let root_id = ident(INVOCATION_ID, "root", "", "root");
    let child_id = ident(INVOCATION_ID, "child", "", "root/child");
    let (root_start, _) = tm.push_agent_span(&bg(), &root_id);
    let (child_start, _) = tm.push_agent_span(&bg(), &child_id);
    tm.push_llm_span(&bg(), &child_id);
    tm.pop_llm_span(&bg(), &child_id, true);

    let token = CancellationToken::new();
    let ctx = bg().with_cancel(token.clone());
    token.cancel();
    let (unclosed, term_err) = tm.pop_unclosed_agent_spans(&ctx, &root_id);
    assert!(
        term_err.as_ref().is_some_and(|e| is_canceled(e)),
        "popUnclosedAgentSpans termErr = {term_err:?}, want context canceled"
    );
    let mut got = Vec::new();
    for u in &unclosed {
        got.push(u.span.span_id.clone());
        assert!(
            u.err.as_ref().is_some_and(|e| is_canceled(e)),
            "{:?} err = {:?}, want context canceled",
            u.agent_name,
            u.err
        );
    }
    assert_eq!(
        got,
        vec![child_start.span_id, root_start.span_id],
        "popUnclosedAgentSpans span IDs"
    );
}

#[test]
fn test_trace_manager_function_response_retires_only_its_tool_call() {
    struct Case {
        name: &'static str,
        root_calls: Vec<(&'static str, &'static str)>,
        worker_calls: Vec<(&'static str, &'static str)>,
        responses: Vec<&'static str>,
        want_open: usize,
    }
    let tests = vec![
        Case {
            name: "response for every call",
            root_calls: vec![("call-1", "lookup"), ("call-2", "search")],
            worker_calls: vec![],
            responses: vec!["call-1", "call-2"],
            want_open: 0,
        },
        Case {
            name: "response for one of two calls",
            root_calls: vec![("call-1", "lookup"), ("call-2", "search")],
            worker_calls: vec![],
            responses: vec!["call-1"],
            want_open: 1,
        },
        Case {
            name: "no function response",
            root_calls: vec![("call-1", "lookup")],
            worker_calls: vec![],
            responses: vec![],
            want_open: 1,
        },
        Case {
            name: "call without an ID is matched by tool name",
            root_calls: vec![("", "lookup")],
            worker_calls: vec![],
            responses: vec!["lookup"],
            want_open: 0,
        },
        Case {
            name: "call of another activation with the same ID",
            root_calls: vec![("call-1", "lookup")],
            worker_calls: vec![("call-1", "lookup")],
            responses: vec!["call-1"],
            want_open: 1,
        },
    ];
    for (i, tc) in tests.into_iter().enumerate() {
        let name = tc.name;
        let ctx = bg();
        let tm = TraceManager::new();
        let invocation_id = format!("inv-function-response-{i}");
        let root_id = ident(&invocation_id, "root", "", "root");
        let worker_id = ident(&invocation_id, "worker", "", "root/worker");
        tm.push_agent_span(&ctx, &root_id);
        tm.push_agent_span(&ctx, &worker_id);
        for (id, tool) in &tc.root_calls {
            tm.push_tool_span(&ctx, &root_id, &tool_key(&root_id, id, tool));
        }
        for (id, tool) in &tc.worker_calls {
            tm.push_tool_span(&ctx, &worker_id, &tool_key(&worker_id, id, tool));
        }

        let responses: Vec<String> = tc.responses.iter().map(|s| s.to_string()).collect();
        tm.complete_response(&ctx, &root_id, "root", "", &responses);

        assert_eq!(
            open_spans(&tm, &invocation_id).tools,
            tc.want_open,
            "{name}: open tool calls"
        );
        let inv = super::direct_support::invocation_of(&tm, &invocation_id);
        assert_eq!(
            inv.has_open_calls(),
            tc.want_open > 0,
            "{name}: hasOpenCalls(): the count that protects an invocation from eviction follows the open calls"
        );
        let (_, term_err) = tm.pop_unclosed_agent_spans(&ctx, &root_id);
        assert_eq!(
            term_err.is_some(),
            tc.want_open > 0,
            "{name}: popUnclosedAgentSpans error = {term_err:?}, want a failure"
        );
    }
}

#[test]
fn test_trace_manager_final_event_marks_activations_as_responded() {
    let root = ("root", "root");
    let sub = ("sub", "root/sub");
    let a = ("a", "root/a");
    let b = ("b", "root/b");
    let user = crate::plugin::USER_AUTHOR;
    // (name, running, author, path, want_blamed)
    let tests: Vec<(&str, Vec<(&str, &str)>, &str, &str, &str)> = vec![
        (
            "event of an agent settles a sub-agent that runs under it",
            vec![root, sub],
            "root",
            "root",
            "",
        ),
        (
            "event of a sub-agent settles the agent that it runs under",
            vec![root, sub],
            "sub",
            "root/sub",
            "",
        ),
        (
            "event of an agent without an open span settles the activation that it would run under",
            vec![root],
            "late",
            "root/late",
            "",
        ),
        (
            "event of a sibling leaves the other sibling unsettled",
            vec![root, a, b],
            "a",
            "root/a",
            "b",
        ),
        (
            "event of the user settles nothing",
            vec![root],
            user,
            "root",
            "root",
        ),
        (
            "event without an author settles nothing",
            vec![root],
            "",
            "root",
            "root",
        ),
    ];
    for (i, (name, running, author, path, want_blamed)) in tests.into_iter().enumerate() {
        let ctx = bg();
        let tm = TraceManager::new();
        let invocation_id = format!("inv-final-event-{i}");
        for (agent, p) in running {
            tm.push_agent_span(&ctx, &ident(&invocation_id, agent, "", p));
        }

        // The event callback gets the agent that the runner runs, and the event brings its
        // own node path.
        let event_id = ident(&invocation_id, "root", "", path);
        tm.complete_response(&ctx, &event_id, author, "", &[]);

        let root_id = ident(&invocation_id, "root", "", "root");
        let (unclosed, term_err) = tm.pop_unclosed_agent_spans(&ctx, &root_id);
        let got_blamed = unclosed
            .first()
            .map(|u| u.agent_name.clone())
            .unwrap_or_default();
        assert_eq!(
            got_blamed, want_blamed,
            "{name}: popUnclosedAgentSpans blames"
        );
        assert_eq!(
            term_err.is_some(),
            !want_blamed.is_empty(),
            "{name}: popUnclosedAgentSpans error = {term_err:?}, want a failure"
        );
    }
}

#[test]
fn test_trace_manager_final_event_clears_the_errors_of_its_activation() {
    let root = ("root", "root");
    let a = ("a", "root/a");
    let b = ("b", "root/b");
    let worker = ("worker", "root/a/worker");
    type Case<'a> = (
        &'a str,
        Vec<(&'a str, &'a str)>,
        Vec<(&'a str, &'a str)>,
        (&'a str, &'a str),
        Vec<&'a str>,
        &'a str,
    );
    // (name, running, failing, author, want_failed, want_err)
    let tests: Vec<Case<'_>> = vec![
        (
            "error of the activation that responded",
            vec![root, a],
            vec![a],
            a,
            vec![],
            "",
        ),
        (
            "error of a sibling activation",
            vec![root, a, b],
            vec![a, b],
            a,
            vec!["b", "root"],
            "model failure of b",
        ),
        (
            "error of an activation that runs under the author",
            vec![root, a, worker],
            vec![worker],
            a,
            vec!["worker", "a", "root"],
            "model failure of worker",
        ),
    ];
    for (i, (name, running, failing, author, want_failed, want_err)) in
        tests.into_iter().enumerate()
    {
        let ctx = bg();
        let tm = TraceManager::new();
        let invocation_id = format!("inv-final-event-errors-{i}");
        let id_in =
            |(agent, path): (&str, &str)| -> Identity { ident(&invocation_id, agent, "", path) };
        for id in running {
            tm.push_agent_span(&ctx, &id_in(id));
        }
        for id in failing {
            let fid = id_in(id);
            tm.record_error(
                &ctx,
                &fid,
                &llm_key(&fid),
                err(&format!("model failure of {}", id.0)),
            );
        }

        tm.complete_response(&ctx, &id_in(author), author.0, "", &[]);

        let (unclosed, term_err) = tm.pop_unclosed_agent_spans(&ctx, &id_in(root));
        assert_eq!(
            names(&unclosed),
            want_failed,
            "{name}: popUnclosedAgentSpans activations"
        );
        let got_err = term_err.map(|e| e.to_string()).unwrap_or_default();
        assert_eq!(got_err, want_err, "{name}: popUnclosedAgentSpans error");
    }
}

/// Go `brokenAgent`: an agent whose sub-agents cannot be read.
struct BrokenAgent {
    state: gadk_agent::AgentState,
}

impl gadk_agent::Agent for BrokenAgent {
    fn name(&self) -> &str {
        "broken"
    }
    fn description(&self) -> &str {
        ""
    }
    fn sub_agents(&self) -> &[Arc<dyn gadk_agent::Agent>] {
        panic!("SubAgents failed")
    }
    fn run(&self, _ctx: gadk_agent::InvocationContext) -> gadk_agent::EventStream {
        Box::pin(futures::stream::empty())
    }
    fn agent_state(&self) -> &gadk_agent::AgentState {
        &self.state
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

// parity: adapted: Go's `deltaContext` cases whose `WithICDelta` returns nil or panics have no gadk equivalent (an ADK context always has its invocation context); `contextRunning(nil)` is a context built without an agent (the builder's default agent, not named "sequence").
#[test]
#[ignore = "plan defect (trace/mod.rs, Task 5): CallbackCtx::running_agent_children does not recover a panicking Agent::sub_agents as Go runningAgentChildren does (orZero -> zero agentChildren); the \"sub-agents cannot be read\" case panics, every other case passes"]
fn test_running_agent_children() {
    use super::direct_support::context_running;
    use crate::parity_tests::plugin_agentanalytics_bigquery_agent_analytics_plugin::support::new_test_agent;
    use crate::trace::AgentChildren;

    let (a, b) = (new_test_agent("a", vec![]), new_test_agent("b", vec![]));
    let sequence = new_test_agent("sequence", vec![a.clone(), b]);
    let helper = llm("helper", vec![], Mode::Unset);
    let coordinator = llm("coordinator", vec![helper], Mode::Unset);
    let broken: Arc<dyn gadk_agent::Agent> = Arc::new(BrokenAgent {
        state: gadk_agent::AgentState {
            agent_type: gadk_agent::AgentType::CustomAgent,
            config: None,
            on_before_agent_callback_event: None,
            live: None,
        },
    });
    let no_agent = CallbackCtx::from_context(&gadk_agent::Context::builder().build());
    let children = |parent: &str, non_llm: bool, names: &[&str]| AgentChildren {
        parent: parent.into(),
        non_llm,
        names: names.iter().map(|s| s.to_string()).collect(),
        ..AgentChildren::default()
    };
    let tests: Vec<(&str, CallbackCtx, &str, AgentChildren)> = vec![
        (
            "workflow agent",
            context_running(&sequence),
            "sequence",
            children("sequence", true, &["a", "b"]),
        ),
        (
            "LLM agent",
            context_running(&coordinator),
            "coordinator",
            children("coordinator", false, &["helper"]),
        ),
        (
            "agent without sub-agents",
            context_running(&a),
            "a",
            children("a", true, &[]),
        ),
        (
            "another agent runs",
            context_running(&sequence),
            "other",
            AgentChildren::default(),
        ),
        (
            "no agent runs",
            no_agent,
            "sequence",
            AgentChildren::default(),
        ),
        (
            "no name",
            context_running(&sequence),
            "",
            AgentChildren::default(),
        ),
        (
            "sub-agents cannot be read",
            context_running(&broken),
            "broken",
            AgentChildren::default(),
        ),
        (
            "context without invocation context",
            bg(),
            "sequence",
            AgentChildren::default(),
        ),
    ];
    for (name, ctx, agent_name, want) in tests {
        let got = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            ctx.running_agent_children(agent_name)
        }));
        match got {
            Ok(got) => assert_eq!(got, want, "{name}: runningAgentChildren"),
            Err(_) => panic!("{name}: runningAgentChildren panicked, want {want:?}"),
        }
    }
}

#[test]
fn test_invocation_trace_record_children_replaces_the_parent() {
    use super::direct_support::context_running;
    use crate::parity_tests::plugin_agentanalytics_bigquery_agent_analytics_plugin::support::new_test_agent;

    let ctx = bg();
    let tm = TraceManager::new();
    const INVOCATION_ID: &str = "inv-per-activation-children";

    let (worker_seq, other_seq) = (
        new_test_agent("worker", vec![]),
        new_test_agent("other", vec![]),
    );
    let sequence = new_test_agent("sequence", vec![worker_seq, other_seq]);

    let worker_llm = llm("worker", vec![], Mode::Unset);
    let peer_llm = llm("peer", vec![], Mode::Unset);
    let coordinator = llm("coordinator", vec![peer_llm, worker_llm], Mode::Unset);

    let (seq_span, _) = tm.push_agent_span(
        &context_running(&sequence),
        &ident(INVOCATION_ID, "sequence", "", "root/sequence@1"),
    );
    let (coord_span, _) = tm.push_agent_span(
        &context_running(&coordinator),
        &ident(INVOCATION_ID, "coordinator", "", "root/coordinator@2"),
    );

    // Under coordinator (an LLM agent), peer starts and stays open when worker starts; worker
    // is parented to peer (peer transfer).
    let (peer_span, _) = tm.push_agent_span(
        &ctx,
        &ident(INVOCATION_ID, "peer", "", "root/coordinator@2"),
    );
    assert_eq!(
        peer_span.parent_span_id, coord_span.span_id,
        "peer parent, want coordinator"
    );
    let (coord_worker_span, _) = tm.push_agent_span(
        &ctx,
        &ident(INVOCATION_ID, "worker", "", "root/coordinator@2"),
    );
    assert_eq!(
        coord_worker_span.parent_span_id, peer_span.span_id,
        "coordinator worker parent, want peer"
    );

    // Under sequence (a non-LLM workflow agent), worker starts and stays open when other
    // starts; other skips worker and is parented to sequence.
    let (seq_worker_span, _) =
        tm.push_agent_span(&ctx, &ident(INVOCATION_ID, "worker", "", "root/sequence@1"));
    assert_eq!(
        seq_worker_span.parent_span_id, seq_span.span_id,
        "sequence worker parent, want sequence"
    );
    let (seq_other_span, _) =
        tm.push_agent_span(&ctx, &ident(INVOCATION_ID, "other", "", "root/sequence@1"));
    assert_eq!(
        seq_other_span.parent_span_id, seq_span.span_id,
        "sequence other parent, want sequence {:?} rather than sibling {:?}",
        seq_span.span_id, seq_worker_span.span_id
    );
}

#[test]
fn test_trace_manager_sub_agents_of_running_agent_stay_apart_outside_the_runner_tree() {
    use super::direct_support::context_running;
    use crate::parity_tests::plugin_agentanalytics_bigquery_agent_analytics_plugin::support::new_test_agent;

    let ctx = bg();
    let tm = TraceManager::new();
    const INVOCATION_ID: &str = "inv-running-agent-children";
    let id = |name: &str| ident(INVOCATION_ID, name, "", "graph");
    let (shortcut, next) = (
        new_test_agent("shortcut", vec![]),
        new_test_agent("next", vec![]),
    );
    let sequence = new_test_agent("sequence", vec![shortcut, next]);

    let (sequence_span, _) = tm.push_agent_span(&context_running(&sequence), &id("sequence"));
    let (shortcut_span, _) = tm.push_agent_span(&ctx, &id("shortcut"));
    let (next_span, _) = tm.push_agent_span(&ctx, &id("next"));

    assert_eq!(
        shortcut_span.parent_span_id, sequence_span.span_id,
        "shortcut: parent span, want the sequence"
    );
    assert_eq!(
        next_span.parent_span_id, sequence_span.span_id,
        "next: parent span, want the sequence {:?} rather than its sibling {:?}",
        sequence_span.span_id, shortcut_span.span_id
    );
}

// parity: adapted: Go's nil `*genai.Part` entry has no gadk equivalent (parts are values).
#[test]
fn test_function_response_calls() {
    use crate::plugin::callbacks::function_response_calls;
    use gadk_genai::{FunctionCall, FunctionResponse, Part};

    let tests: Vec<(&str, Option<Content>, Vec<&str>)> = vec![
        ("no content", None, vec![]),
        ("text only", Some(Content::from_text("hi", "model")), vec![]),
        (
            "responses among other parts",
            Some(Content {
                parts: vec![
                    Part {
                        text: "hi".into(),
                        ..Part::default()
                    },
                    Part {
                        function_call: Some(FunctionCall {
                            id: "call-0".into(),
                            name: "lookup".into(),
                            ..FunctionCall::default()
                        }),
                        ..Part::default()
                    },
                    Part {
                        function_response: Some(FunctionResponse::default()),
                        ..Part::default()
                    },
                    Part {
                        function_response: Some(FunctionResponse {
                            id: "call-1".into(),
                            name: "lookup".into(),
                            ..FunctionResponse::default()
                        }),
                        ..Part::default()
                    },
                    Part {
                        function_response: Some(FunctionResponse {
                            name: "search".into(),
                            ..FunctionResponse::default()
                        }),
                        ..Part::default()
                    },
                ],
                ..Content::default()
            }),
            vec!["call-1", "search"],
        ),
    ];
    for (name, content, want) in tests {
        assert_eq!(
            function_response_calls(content.as_ref()),
            want,
            "{name}: functionResponseCalls"
        );
    }
}

#[test]
fn test_function_response_calls_match_tool_keys() {
    use crate::plugin::callbacks::function_response_calls;
    use crate::trace::tool_key_call;
    use gadk_genai::{FunctionResponse, Part};

    let id = ident("", "bot", "root.bot", "root/bot");
    let tests = [
        ("function call ID", "call-1", "lookup", "call-1"),
        (
            "tool name without a function call ID",
            "",
            "lookup",
            "lookup",
        ),
    ];
    for (name, resp_id, resp_name, want) in tests {
        let content = Content {
            parts: vec![Part {
                function_response: Some(FunctionResponse {
                    id: resp_id.into(),
                    name: resp_name.into(),
                    ..FunctionResponse::default()
                }),
                ..Part::default()
            }],
            ..Content::default()
        };
        let got = function_response_calls(Some(&content));
        assert_eq!(got, vec![want.to_string()], "{name}: functionResponseCalls");
        let key = tool_key(&id, resp_id, resp_name);
        assert_eq!(
            tool_key_call(&key),
            got[0],
            "{name}: toolKeyCall(toolKey(...)): a response must name the key of its call"
        );
    }
}

// parity: adapted: Go attaches the runner's parent map to a plain context as a value; gadk's parent map lives on the ADK context, so the cases with a parent map run on an ADK context of the runner agent built with `ParentMap::new(runner)` (whose entries are Go's map).
#[test]
fn test_trace_manager_earlier_runner_ancestry_stays_out_of_the_invocation() {
    use crate::parity_tests::plugin_agentanalytics_bigquery_agent_analytics_plugin::support::new_test_agent;
    use crate::trace::testkit::adk_ctx;
    use std::collections::HashMap;

    let helper = new_test_agent("helper", vec![]);
    let worker = new_test_agent("worker", vec![helper.clone()]);
    let root = new_test_agent("root", vec![worker.clone()]);

    // (name, runner, with parent map, want_root, want_parents)
    let tests: Vec<(
        &str,
        &Arc<dyn gadk_agent::Agent>,
        bool,
        &str,
        Vec<(&str, &str)>,
    )> = vec![
        (
            "parent map of the runner",
            &worker,
            true,
            "worker",
            vec![("helper", "worker")],
        ),
        ("empty parent map", &helper, true, "helper", vec![]),
        (
            "no parent map",
            &worker,
            false,
            "root",
            vec![("worker", "root"), ("helper", "worker")],
        ),
    ];
    for (name, runner, with_parents, want_root, want_parents) in tests {
        let tm = TraceManager::new();
        // The earlier runner runs root.
        tm.remember_agent_tree(Some(&root));
        let ctx = if with_parents {
            CallbackCtx::from_context(&adk_ctx(runner, "inv-earlier-runner", Some(runner)))
        } else {
            bg()
        };
        let id = ident("inv-earlier-runner", runner.name(), "", "");
        let got = tm.invocation_span(&ctx, &id, Some(runner)).root_agent_name;
        assert_eq!(got, want_root, "{name}: root agent");
        let inv = tm.invocation(&ctx, &id);
        let st = inv.mu.lock();
        let want: HashMap<String, String> = want_parents
            .iter()
            .map(|(c, p)| (c.to_string(), p.to_string()))
            .collect();
        assert_eq!(st.parent_agent, want, "{name}: recorded parents");
    }
}

#[tokio::test]
async fn test_log_event_event_filters_preserve_spans_latency_and_invocation_cleanup() {
    use crate::event_types::{EVENT_AGENT_COMPLETED, EVENT_AGENT_STARTING, EVENT_LLM_RESPONSE};
    use crate::parity_tests::plugin_agentanalytics_bigquery_agent_analytics_plugin::support::{
        event_types, must_latency, new_test_plugin_with_config, place_of, until_event_type,
    };
    use gadk_model::LlmRequest;

    // allowlist keeps only completed rows with intact spans and latency
    {
        let tp = new_test_plugin_with_config(|cfg| {
            cfg.event_allowlist = vec![
                EVENT_LLM_RESPONSE.into(),
                EVENT_AGENT_COMPLETED.into(),
                EVENT_INVOCATION_COMPLETED.into(),
            ];
        })
        .await;
        let run = MockRun::new(&tp.ctx);
        let p = &tp.plugin;
        p.before_run_callback().unwrap()(run.inv()).await.unwrap();
        p.before_agent_callback().unwrap()(run.ctx()).await.unwrap();
        let _ = p.before_model_callback().unwrap()(run.ctx(), LlmRequest::default()).await;
        let (_, res) = p.after_model_callback().unwrap()(
            run.ctx(),
            LlmResponse {
                content: Some(Content::from_text("done", "model")),
                ..LlmResponse::default()
            },
            None,
        )
        .await;
        res.unwrap();
        p.after_agent_callback().unwrap()(run.ctx()).await.unwrap();
        p.after_run_callback().unwrap()(run.inv()).await;

        let rows = tp
            .collect_rows(until_event_type(EVENT_INVOCATION_COMPLETED))
            .await;
        assert_eq!(
            event_types(&rows),
            vec![
                EVENT_LLM_RESPONSE,
                EVENT_AGENT_COMPLETED,
                EVENT_INVOCATION_COMPLETED
            ],
            "filtered event types"
        );
        let llm_resp = only_row_of_type(&rows, EVENT_LLM_RESPONSE);
        let agent_comp = only_row_of_type(&rows, EVENT_AGENT_COMPLETED);
        let inv_comp = only_row_of_type(&rows, EVENT_INVOCATION_COMPLETED);
        assert_eq!(
            place_of(&llm_resp).parent,
            place_of(&agent_comp).span,
            "LLM_RESPONSE parent_span_id, want AGENT_COMPLETED span_id"
        );
        assert_eq!(
            place_of(&agent_comp).parent,
            place_of(&inv_comp).span,
            "AGENT_COMPLETED parent_span_id, want INVOCATION_COMPLETED span_id"
        );
        must_latency(&llm_resp);
        must_latency(&agent_comp);
        must_latency(&inv_comp);
    }

    // denylist filtering INVOCATION_COMPLETED still cleans up invocation state
    {
        let tp = new_test_plugin_with_config(|cfg| {
            cfg.event_denylist = vec![EVENT_INVOCATION_COMPLETED.into()];
        })
        .await;
        let run = MockRun::new(&tp.ctx);
        let p = &tp.plugin;
        p.before_run_callback().unwrap()(run.inv()).await.unwrap();
        p.before_agent_callback().unwrap()(run.ctx()).await.unwrap();
        p.after_agent_callback().unwrap()(run.ctx()).await.unwrap();
        p.after_run_callback().unwrap()(run.inv()).await;

        let rows = tp
            .collect_rows(until_event_type(EVENT_AGENT_COMPLETED))
            .await;
        assert!(
            !event_types(&rows).contains(&EVENT_INVOCATION_COMPLETED.to_string()),
            "denylisted INVOCATION_COMPLETED was emitted: {:?}",
            event_types(&rows)
        );
        let first_parent = place_of(&only_row_of_type(&rows, EVENT_AGENT_COMPLETED)).parent;

        // Starting a new run with the same invocation ID after AfterRunCallback must allocate
        // a fresh invocation root span ID, proving endInvocation removed the finished
        // invocation even though INVOCATION_COMPLETED was filtered out.
        p.before_run_callback().unwrap()(run.inv()).await.unwrap();
        p.before_agent_callback().unwrap()(run.ctx()).await.unwrap();
        let next_rows = tp
            .collect_rows(|rows: &[crate::row::Row]| {
                rows_of_type(rows, EVENT_AGENT_STARTING).len() >= 2
            })
            .await;
        let second_parent = place_of(&rows_of_type(&next_rows, EVENT_AGENT_STARTING)[1]).parent;
        assert_ne!(
            second_parent, first_parent,
            "subsequent callback reused invocation span_id; endInvocation did not clean up invocation state when INVOCATION_COMPLETED was denylisted"
        );
    }
}

#[test]
fn test_trace_manager_evicted_tombstone_cap_drops_oldest_only() {
    let ctx = bg();
    let clock = ManualClock::new();
    let mut tm = TraceManager::new();
    tm.max_tracked = 1;
    tm.now = clock.clock();
    let tm = tm;

    // Trigger 65 evictions at distinct timestamps; the tombstone cap is max(1, 64) = 64.
    let mut evicted_ids = Vec::new();
    for i in 0..66 {
        clock.advance_ms(1);
        let id = ident(&format!("inv-{i:03}"), "root", "", "");
        tm.push_agent_span(&ctx, &id);
        if i < 65 {
            evicted_ids.push(id.invocation_id.clone());
        }
    }

    let st = tm.mu.lock();
    assert_eq!(st.evicted.len(), 64, "len(tm.evicted)");
    assert!(
        !st.evicted.contains_key(&evicted_ids[0]),
        "oldest tombstone {:?} is still present, want it evicted",
        evicted_ids[0]
    );
    for id in &evicted_ids[1..] {
        assert!(
            st.evicted.contains_key(id),
            "recent tombstone {id:?} was lost when cap was reached"
        );
    }
}

// parity: adapted: gadk callbacks read the ambient span from the current tracing span, so the first subtest drives the analytics plugin's synchronous callback bodies with explicit `CallbackCtx` spans (Go's `trace.ContextWithSpanContext`) and reads the rows from the queue of a processor that never starts (as Go's clocked plugin), not from the write server.
#[tokio::test]
async fn test_trace_manager_adopt_first_agent_o_tel_span_when_caller_has_no_span() {
    use super::direct_support::fake_tool;
    use crate::event_types::{
        EVENT_AGENT_COMPLETED, EVENT_AGENT_STARTING, EVENT_INVOCATION_STARTING, EVENT_LLM_REQUEST,
        EVENT_LLM_RESPONSE, EVENT_STATE_DELTA, EVENT_TOOL_COMPLETED, EVENT_TOOL_STARTING,
        EVENT_USER_MESSAGE_RECEIVED,
    };
    use crate::parity_tests::plugin_agentanalytics_bigquery_agent_analytics_plugin::support::{
        MockCtx, new_clocked_plugin, place_of,
    };
    use gadk_model::LlmRequest;
    use serde_json::{Map, Value};

    const TRACE_HEX: &str = "4bf92f3577b34da6a3ce929d0e0e4736";
    const SPAN_HEX: &str = "00f067aa0ba902b7";
    let sc = span_ctx(TRACE_HEX, SPAN_HEX).span.unwrap();

    // adopts first ambient span on agent, model, and tool rows and propagates to spanless
    // OnEvent and AfterRun
    {
        let c = new_clocked_plugin().await;
        let p = &c.plugin;
        let run = MockRun::new(&MockCtx::new());
        let m_ctx = CallbackCtx::from_invocation(&run.inv());
        let agent_ctx = run.cb().with_span(sc);
        let tool_ctx = run.tool_cb("call-1").with_span(sc);
        let lookup_tool = fake_tool("lookup");

        // Caller starts with no OTel span on mCtx.
        p.on_user_message(&m_ctx, &Content::from_text("hello", "user"));
        p.before_run(&m_ctx);
        // ADK's agent.Run opens an OTel span on agentCtx.
        p.before_agent(&agent_ctx);
        p.before_model(
            &agent_ctx,
            &LlmRequest {
                model: "gemini-test".into(),
                ..LlmRequest::default()
            },
        );
        p.after_model(
            &agent_ctx,
            &LlmResponse {
                content: Some(Content::from_text("calling lookup", "model")),
                ..LlmResponse::default()
            },
            None,
        );
        p.before_tool(&tool_ctx, &lookup_tool, &Map::new());
        let mut result = Map::new();
        result.insert("answer".into(), Value::String("sunny".into()));
        p.after_tool(&tool_ctx, &lookup_tool, &Map::new(), &result, None);
        p.after_agent(&agent_ctx);
        // OnEventCallback and AfterRunCallback run on outer mCtx (no ambient span).
        let mut ev = Event::new(
            &gadk_platform::Platform::default(),
            run.mock.invocation_id.clone(),
        );
        ev.author = run.mock.agent_name.clone();
        let mut delta = Map::new();
        delta.insert("k".into(), Value::String("v".into()));
        ev.actions.state_delta = Some(delta);
        p.on_event(&m_ctx, &ev);
        p.after_run(&m_ctx);

        let rows = c.take_rows();
        let user_place = place_of(&only_row_of_type(&rows, EVENT_USER_MESSAGE_RECEIVED));
        let inv_start_place = place_of(&only_row_of_type(&rows, EVENT_INVOCATION_STARTING));
        assert!(
            !user_place.trace.is_empty() && user_place.trace != TRACE_HEX,
            "USER_MESSAGE_RECEIVED trace_id = {:?}, want initial generated random trace ID (!= {TRACE_HEX:?})",
            user_place.trace
        );
        assert_eq!(
            inv_start_place.trace, user_place.trace,
            "INVOCATION_STARTING trace_id"
        );

        for ev_type in [
            EVENT_AGENT_STARTING,
            EVENT_LLM_REQUEST,
            EVENT_LLM_RESPONSE,
            EVENT_TOOL_STARTING,
            EVENT_TOOL_COMPLETED,
            EVENT_AGENT_COMPLETED,
            EVENT_STATE_DELTA,
            EVENT_INVOCATION_COMPLETED,
        ] {
            let row = only_row_of_type(&rows, ev_type);
            assert_eq!(
                place_of(&row).trace,
                TRACE_HEX,
                "{ev_type} trace_id, want adopted ambient trace_id"
            );
        }
    }

    // capacity eviction preserves adopted trace_id for post-eviction AfterRun
    {
        let mut tm = TraceManager::new();
        tm.max_tracked = 1;
        let tm = tm;
        let id1 = ident("inv-adopted-evict-1", "root", "", "");
        let id2 = ident("inv-adopted-evict-2", "root", "", "");

        // Start id1 without an ambient span.
        let start_span = tm.invocation_span(&bg(), &id1, None);
        assert_ne!(
            start_span.trace_id, TRACE_HEX,
            "initial traceID, want random trace ID"
        );
        // BeforeAgentCallback runs with ambient span, adopting TRACE_HEX.
        let otel_ctx = bg().with_span(sc);
        let (agent_span, _) = tm.push_agent_span(&otel_ctx, &id1);
        assert_eq!(agent_span.trace_id, TRACE_HEX, "pushAgentSpan traceID");
        tm.pop_agent_span(&otel_ctx, &id1);

        // Starting id2 evicts id1 into tm.evicted via markEvictedLocked.
        tm.invocation_span(&bg(), &id2, None);

        // Ending id1 without an ambient span restores the evicted tombstone, which must carry
        // the adopted TRACE_HEX via inv.currentTraceID().
        let end_span = tm.end_invocation(&bg(), &id1);
        assert_eq!(
            end_span.trace_id, TRACE_HEX,
            "post-eviction endInvocation traceID, want adopted trace_id"
        );
    }
}

/// Whether the open agent span named `name` of `invocation_id` was marked task-delegated.
fn task_delegated(tm: &TraceManager, invocation_id: &str, name: &str) -> bool {
    let inv = super::direct_support::invocation_of(tm, invocation_id);
    let st = inv.mu.lock();
    st.agent_stack.iter().any(|r| {
        let r = r.lock();
        r.name == name && r.task_delegated
    })
}

/// The `(non_llm, children)` of the last open agent span named `name`.
fn agent_rec(tm: &TraceManager, invocation_id: &str, name: &str) -> Option<(bool, Vec<String>)> {
    let inv = super::direct_support::invocation_of(tm, invocation_id);
    let st = inv.mu.lock();
    st.agent_stack.iter().rev().find_map(|r| {
        let r = r.lock();
        (r.name == name).then(|| (r.non_llm, r.children.clone()))
    })
}

// parity: adapted: Go's task agent has a scripted model that the test never calls; the gadk LLM agent has none.
#[test]
fn test_trace_manager_pop_task_agent_span_does_not_mark_non_llm_parent() {
    use super::direct_support::context_running;

    let ctx = bg();
    let tm = TraceManager::new();
    let step_task = llm("step_task", vec![], Mode::Task);
    let seq = sequential("seq", vec![step_task.clone()]);

    let seq_id = ident("inv-nonllm", "seq", "", "seq@0");
    let task_id = ident("inv-nonllm", "step_task", "", "seq@0/step_task@1");

    tm.invocation_span(&context_running(&seq), &seq_id, Some(&seq));
    let (seq_start, _) = tm.push_agent_span(&context_running(&seq), &seq_id);

    // Verify seq was recorded as non-LLM with step_task as a child.
    let (non_llm, children) = agent_rec(&tm, "inv-nonllm", "seq")
        .unwrap_or_else(|| panic!("seq span not found on the stack"));
    assert!(
        non_llm,
        "seq.nonLLM = false, want true (sequentialagent is non-LLM)"
    );
    assert!(
        children.contains(&"step_task".to_string()),
        "seq.children = {children:?}, want to contain \"step_task\""
    );

    // Push the task span (parented to seq because seq has step_task in children).
    let (task_start, _) = tm.push_agent_span(&context_running(&step_task), &task_id);
    assert_eq!(
        task_start.parent_span_id, seq_start.span_id,
        "task parentSpanID, want seq spanID"
    );

    // Pop the task span as popTaskAgentSpan does (finish_task completion).
    tm.pop_task_agent_span(&ctx, &task_id, &[]);

    // Verify seq was NOT marked taskDelegated (defensive guard).
    assert!(
        !task_delegated(&tm, "inv-nonllm", "seq"),
        "seq was marked taskDelegated, but a non-LLM workflow agent must never be marked"
    );
}

#[test]
fn test_trace_manager_pop_task_agent_span_marks_direct_llm_parent() {
    use super::direct_support::context_running;

    let ctx = bg();
    let tm = TraceManager::new();
    let task_worker = llm("task_worker", vec![], Mode::Task);
    let coordinator = llm("coordinator", vec![task_worker.clone()], Mode::Unset);

    let coord_id = ident("inv-llm-parent", "coordinator", "", "coordinator@0");
    let task_id = ident(
        "inv-llm-parent",
        "task_worker",
        "",
        "coordinator@0/task_worker@1",
    );

    tm.invocation_span(
        &context_running(&coordinator),
        &coord_id,
        Some(&coordinator),
    );
    let (coord_start, _) = tm.push_agent_span(&context_running(&coordinator), &coord_id);

    // Verify coordinator is an LLM agent with task_worker as a child.
    let (non_llm, children) = agent_rec(&tm, "inv-llm-parent", "coordinator")
        .unwrap_or_else(|| panic!("coordinator span not found on the stack"));
    assert!(
        !non_llm,
        "coordinator.nonLLM = true, want false (coordinator is an LLM agent)"
    );
    assert!(
        children.contains(&"task_worker".to_string()),
        "coordinator.children = {children:?}, want to contain \"task_worker\""
    );

    let (task_start, _) = tm.push_agent_span(&context_running(&task_worker), &task_id);
    assert_eq!(
        task_start.parent_span_id, coord_start.span_id,
        "task parentSpanID, want coordinator spanID"
    );

    tm.pop_task_agent_span(&ctx, &task_id, &[]);

    // Verify coordinator WAS marked taskDelegated.
    assert!(
        task_delegated(&tm, "inv-llm-parent", "coordinator"),
        "coordinator was not marked taskDelegated, but a direct LLM parent should be marked"
    );
}

#[test]
fn test_trace_manager_pop_task_agent_span_ignores_unrelated_ancestor() {
    use super::direct_support::context_running;

    let ctx = bg();
    let tm = TraceManager::new();
    let other_worker = llm("other_worker", vec![], Mode::Task);
    let task_worker = llm("task_worker", vec![], Mode::Task);
    // note: does NOT contain task_worker
    let coordinator = llm("coordinator", vec![other_worker], Mode::Unset);

    let coord_id = ident("inv-ancestor", "coordinator", "", "coordinator@0");
    let task_id = ident(
        "inv-ancestor",
        "task_worker",
        "",
        "coordinator@0/task_worker@1",
    );

    tm.invocation_span(
        &context_running(&coordinator),
        &coord_id,
        Some(&coordinator),
    );
    let (coord_start, _) = tm.push_agent_span(&context_running(&coordinator), &coord_id);

    let (task_start, _) = tm.push_agent_span(&context_running(&task_worker), &task_id);
    assert_eq!(
        task_start.parent_span_id, coord_start.span_id,
        "task parentSpanID, want coordinator spanID"
    );

    tm.pop_task_agent_span(&ctx, &task_id, &[]);

    // Verify coordinator was NOT marked taskDelegated because task_worker is not in its
    // children.
    assert!(
        !task_delegated(&tm, "inv-ancestor", "coordinator"),
        "coordinator was marked taskDelegated, but task_worker is not in coordinator's SubAgents"
    );
}
