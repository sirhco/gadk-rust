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
//! 21 Go tests from `TestTraceManager_Runner` to
//! `TestRunner_SequentialSiblingShortcutThenCustomChildFails_BlamesOnlyFailingChild` in the Go
//! file's order that run a gadk `Runner` (the exact list is in the 7b-3a plan's contracts, Appendix
//! A).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use gadk_agent::RunConfig;
use gadk_engine::llm::LlmAgentConfig;
use gadk_session::SessionService as _;
use serde_json::{Value, json};

use super::runner_support::*;
use crate::event_types::*;
use crate::parity_tests::plugin_agentanalytics_bigquery_agent_analytics_plugin::support::*;
use crate::trace::testkit::sequential;

#[tokio::test]
async fn test_trace_manager_runner() {
    let tp = setup_test_plugin().await;
    let lookup_tool = function_tool("lookup", "Looks up the weather", |_ctx, args| async move {
        let city = args.get("city").and_then(Value::as_str).unwrap_or_default();
        Ok(json!({"weather": format!("sunny in {city}")})
            .as_object()
            .cloned()
            .unwrap())
    });
    let helper = llm_agent(LlmAgentConfig {
        name: "helper".into(),
        description: "Helps".into(),
        ..Default::default()
    });
    let model = scripted_llm(vec![
        calls(vec![
            function_call_part("lookup", json!({"city": "Paris"})),
            function_call_part("lookup", json!({"city": "Rome"})),
        ]),
        final_text("Sunny in both."),
    ]);
    let planner = llm_agent(LlmAgentConfig {
        name: "planner".into(),
        description: "Plans trips".into(),
        model: Some(model),
        instruction: "Plan the trip.".into(),
        tools: vec![lookup_tool],
        sub_agents: vec![helper],
        ..Default::default()
    });
    let r = new_test_runner(planner, vec![tp.plugin.clone()]);
    let (_, errs) = run_turn(
        &r,
        "user-1",
        "session-1",
        "What is the weather?",
        RunConfig::default(),
    )
    .await;
    assert!(errs.is_empty(), "Run() errors: {errs:?}");

    let rows = RowCursor::default()
        .collect(&tp, until_event_type(EVENT_INVOCATION_COMPLETED))
        .await;
    let mut got_counts: BTreeMap<String, usize> = BTreeMap::new();
    for et in event_types(&rows) {
        *got_counts.entry(et).or_insert(0) += 1;
    }
    let want_counts: BTreeMap<String, usize> = [
        (EVENT_USER_MESSAGE_RECEIVED, 1),
        (EVENT_INVOCATION_STARTING, 1),
        (EVENT_AGENT_STARTING, 1),
        (EVENT_LLM_REQUEST, 2),
        (EVENT_LLM_RESPONSE, 2),
        (EVENT_TOOL_STARTING, 2),
        (EVENT_TOOL_COMPLETED, 2),
        (EVENT_AGENT_COMPLETED, 1),
        (EVENT_INVOCATION_COMPLETED, 1),
        // The final answer session event emits AGENT_RESPONSE.
        (EVENT_AGENT_RESPONSE, 1),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v))
    .collect();
    assert_eq!(
        got_counts,
        want_counts,
        "event type counts; event types: {:?}",
        event_types(&rows)
    );

    let inv = place_of(&only_row_of_type(&rows, EVENT_INVOCATION_STARTING));
    assert!(
        event_id_pattern().is_match(&inv.trace)
            && span_id_pattern().is_match(&inv.span)
            && inv.parent.is_empty(),
        "INVOCATION_STARTING place = {inv:?}, want a new trace and a root span"
    );
    for et in [EVENT_USER_MESSAGE_RECEIVED, EVENT_INVOCATION_COMPLETED] {
        assert_eq!(
            place_of(&only_row_of_type(&rows, et)),
            inv,
            "{et} place, want the invocation span"
        );
    }
    let agent_place = place_of(&only_row_of_type(&rows, EVENT_AGENT_STARTING));
    assert!(
        agent_place.trace == inv.trace
            && span_id_pattern().is_match(&agent_place.span)
            && agent_place.parent == inv.span,
        "AGENT_STARTING place = {agent_place:?}, want a span under the invocation span {}",
        inv.span
    );
    assert_eq!(
        place_of(&only_row_of_type(&rows, EVENT_AGENT_COMPLETED)),
        agent_place,
        "AGENT_COMPLETED place"
    );

    // Every model and tool call has a span of its own under the agent.
    let mut spans: BTreeSet<String> = [inv.span.clone(), agent_place.span.clone()].into();
    let mut check_call_span = |name: &str, place: &TracePlace| {
        assert!(
            place.trace == inv.trace && place.parent == agent_place.span,
            "{name} place = {place:?}, want a span under the agent span {}",
            agent_place.span
        );
        assert!(
            spans.insert(place.span.clone()),
            "{name} span {:?} is not a span of its own",
            place.span
        );
    };
    let request_rows = rows_of_type(&rows, EVENT_LLM_REQUEST);
    let response_rows = rows_of_type(&rows, EVENT_LLM_RESPONSE);
    for (i, req) in request_rows.iter().enumerate() {
        let place = place_of(req);
        check_call_span(&format!("LLM_REQUEST {i}"), &place);
        assert_eq!(
            place_of(&response_rows[i]),
            place,
            "LLM_RESPONSE {i} place, want the place of its request"
        );
    }
    let mut started: HashMap<String, TracePlace> = HashMap::new();
    for row in rows_of_type(&rows, EVENT_TOOL_STARTING) {
        let call_id = function_call_id_of(&row);
        check_call_span(&format!("TOOL_STARTING of call {call_id}"), &place_of(&row));
        started.insert(call_id, place_of(&row));
    }
    assert_eq!(
        started.len(),
        2,
        "TOOL_STARTING rows have function call IDs {:?}, want 2 distinct IDs",
        started.keys().collect::<Vec<_>>()
    );
    for row in rows_of_type(&rows, EVENT_TOOL_COMPLETED) {
        let call_id = function_call_id_of(&row);
        let want = started.get(&call_id).unwrap_or_else(|| {
            panic!("TOOL_COMPLETED row of call {call_id:?} has no TOOL_STARTING row")
        });
        assert_eq!(
            &place_of(&row),
            want,
            "TOOL_COMPLETED place of call {call_id:?}"
        );
    }

    // The session events of the agent are placed in its span.
    for row in &rows {
        if lookup(
            &json_object_column(row, "attributes"),
            &["adk", "source_event_id"],
        )
        .is_none()
        {
            continue;
        }
        assert_eq!(
            place_of(row),
            agent_place,
            "{} row of a session event: place",
            str_col(row, "event_type")
        );
    }

    let agent_starting = only_row_of_type(&rows, EVENT_AGENT_STARTING);
    let content = json_column(&agent_starting, "content").unwrap_or(Value::Null);
    check_json("AGENT_STARTING content", &content, r#""Plan the trip.""#).unwrap();
    check_attr(&agent_starting, r#""Plans trips""#, &["agent_description"]);
    check_attr(&agent_starting, r#""scripted-model""#, &["model"]);
    check_attr(&agent_starting, r#"["helper"]"#, &["sub_agents"]);

    let want_latency_keys: HashMap<&str, Vec<&str>> = [
        (
            EVENT_LLM_RESPONSE,
            vec!["time_to_first_token_ms", "total_ms"],
        ),
        (EVENT_TOOL_COMPLETED, vec!["total_ms"]),
        (EVENT_AGENT_COMPLETED, vec!["total_ms"]),
        (EVENT_INVOCATION_COMPLETED, vec!["total_ms"]),
    ]
    .into();
    for row in &rows {
        let et = str_col(row, "event_type");
        check_attr(row, r#""planner""#, &["root_agent_name"]);
        let latency = row_latency(row).unwrap_or_default();
        let got: Vec<&str> = latency.keys().map(String::as_str).collect();
        assert_eq!(
            want_latency_keys
                .get(et.as_str())
                .cloned()
                .unwrap_or_default(),
            got,
            "{et} row: latency_ms keys"
        );
        for (key, ms) in &latency {
            assert!(*ms >= 0, "{et} row: latency_ms.{key} = {ms}, want >= 0");
        }
    }
}

#[tokio::test]
async fn test_trace_manager_sequential_sibling_short_circuited_in_before_agent() {
    let tp = setup_test_plugin().await;
    let shortcut = llm_agent(LlmAgentConfig {
        name: "shortcut".into(),
        before_agent_callbacks: vec![answering_before_agent(text_content(
            "model",
            "short-circuited",
        ))],
        ..Default::default()
    });
    let next = llm_agent(LlmAgentConfig {
        name: "next".into(),
        model: Some(scripted_llm(vec![final_text("done")])),
        ..Default::default()
    });
    let seq = sequential("sequence", vec![shortcut, next]);
    let r = new_test_runner(seq, vec![tp.plugin.clone()]);
    let (_, errs) = run_turn(&r, "user-1", "session-1", "go", RunConfig::default()).await;
    assert!(errs.is_empty(), "Run() errors: {errs:?}");

    let rows = RowCursor::default()
        .collect(&tp, until_event_type(EVENT_INVOCATION_COMPLETED))
        .await;
    let by_agent = start_by_agent(&rows);
    let seq_place = by_agent.get("sequence").cloned().unwrap_or_default();
    let shortcut_place = by_agent.get("shortcut").cloned().unwrap_or_default();
    let next_place = by_agent.get("next").cloned().unwrap_or_default();
    assert_eq!(
        shortcut_place.parent, seq_place.span,
        "shortcut AGENT_STARTING parent_span_id, want sequence span"
    );
    assert_eq!(
        next_place.parent, seq_place.span,
        "next AGENT_STARTING parent_span_id, want sequence span (not short-circuited sibling {:?})",
        shortcut_place.span
    );
    for row in rows_of_type(&rows, EVENT_AGENT_COMPLETED) {
        let name = agent_of(&row);
        assert_eq!(
            Some(&place_of(&row)),
            by_agent.get(&name),
            "{name} AGENT_COMPLETED place"
        );
    }
}

#[tokio::test]
async fn test_trace_manager_sub_agent_transfer_continuation_turn() {
    let tp = setup_test_plugin().await;
    let helper_llm = scripted_llm(vec![
        // Turn 1: helper answers after planner transfers to it.
        final_text("Helper answered turn 1."),
        // Turn 2: Runner starts at helper; helper transfers back to planner.
        transfer_to("planner"),
    ]);
    let planner_llm = scripted_llm(vec![
        // Turn 1: planner transfers to helper.
        transfer_to("helper"),
        // Turn 2: planner answers after helper transfers back to it.
        final_text("Planner answered turn 2."),
    ]);
    let helper = llm_agent(LlmAgentConfig {
        name: "helper".into(),
        description: "Helps".into(),
        model: Some(helper_llm),
        instruction: "Help the user.".into(),
        ..Default::default()
    });
    let planner = llm_agent(LlmAgentConfig {
        name: "planner".into(),
        description: "Plans trips".into(),
        model: Some(planner_llm),
        instruction: "Plan the trip.".into(),
        sub_agents: vec![helper],
        ..Default::default()
    });
    let r = new_test_runner(planner, vec![tp.plugin.clone()]);
    let mut cursor = RowCursor::default();

    // Turn 1: planner -> helper.
    let (_, errs) = run_turn(&r, "user-1", "session-1", "Turn 1", RunConfig::default()).await;
    assert!(errs.is_empty(), "Turn 1 Run() errors: {errs:?}");
    cursor
        .collect(&tp, until_event_type(EVENT_INVOCATION_COMPLETED))
        .await;

    // Turn 2: Runner starts at helper (last author of Turn 1), which transfers back to planner.
    let (_, errs) = run_turn(&r, "user-1", "session-1", "Turn 2", RunConfig::default()).await;
    assert!(errs.is_empty(), "Turn 2 Run() errors: {errs:?}");
    let turn2 = cursor
        .collect(&tp, until_event_type(EVENT_INVOCATION_COMPLETED))
        .await;
    for row in &turn2 {
        check_attr(row, r#""planner""#, &["root_agent_name"]);
    }
    let planner_start = rows_of_type(&turn2, EVENT_AGENT_STARTING)
        .into_iter()
        .find(|r| agent_of(r) == "planner")
        .unwrap_or_else(|| {
            panic!(
                "Turn 2 rows have no AGENT_STARTING for planner; event types: {:?}",
                event_types(&turn2)
            )
        });
    let content = json_column(&planner_start, "content")
        .expect("Turn 2 planner AGENT_STARTING has no content");
    check_json(
        "Turn 2 planner AGENT_STARTING content",
        &content,
        r#""Plan the trip.""#,
    )
    .unwrap();
    check_attr(&planner_start, r#""Plans trips""#, &["agent_description"]);
    check_attr(&planner_start, r#""scripted-model""#, &["model"]);
}

#[tokio::test]
async fn test_trace_manager_cold_persisted_session_resume_resolves_configured_root() {
    let sessions = Arc::new(gadk_session::InMemorySessionService::new());
    let created = sessions
        .create(gadk_session::CreateRequest {
            app_name: "test-app".into(),
            user_id: "user-1".into(),
            session_id: Some("persisted-session".into()),
            ..Default::default()
        })
        .await
        .expect("create session");
    let mut prior = gadk_session::Event::new(&gadk_platform::Platform::default(), "inv-turn-1");
    prior.author = "helper".into();
    prior.response = gadk_model::LlmResponse {
        content: Some(text_content("model", "Helper handled turn 1.")),
        turn_complete: true,
        ..Default::default()
    };
    sessions
        .append_event(created.as_ref(), &mut prior)
        .await
        .expect("append event");

    // Brand-new plugin instance that has never seen Turn 1.
    let tp = setup_test_plugin().await;
    let helper = llm_agent(LlmAgentConfig {
        name: "helper".into(),
        description: "Sub-agent helper".into(),
        model: Some(scripted_llm(vec![transfer_to("configured_root")])),
        instruction: "Help the user.".into(),
        ..Default::default()
    });
    let configured_root = llm_agent(LlmAgentConfig {
        name: "configured_root".into(),
        description: "Root coordinator".into(),
        model: Some(scripted_llm(vec![final_text(
            "Root answered on cold resume.",
        )])),
        instruction: "Coordinate everything.".into(),
        sub_agents: vec![helper],
        ..Default::default()
    });
    let r = runner_with(
        "test-app",
        configured_root,
        sessions,
        false,
        vec![tp.plugin.clone()],
    );
    let (_, errs) = run_turn(
        &r,
        "user-1",
        "persisted-session",
        "Resume turn 2",
        RunConfig::default(),
    )
    .await;
    assert!(errs.is_empty(), "r.Run errors: {errs:?}");

    let rows = RowCursor::default()
        .collect(&tp, until_event_type(EVENT_INVOCATION_COMPLETED))
        .await;
    for row in &rows {
        check_attr(row, r#""configured_root""#, &["root_agent_name"]);
    }
    let root_start = rows_of_type(&rows, EVENT_AGENT_STARTING)
        .into_iter()
        .find(|r| agent_of(r) == "configured_root")
        .unwrap_or_else(|| {
            panic!(
                "no AGENT_STARTING row for configured_root; event types: {:?}",
                event_types(&rows)
            )
        });
    let content =
        json_column(&root_start, "content").expect("configured_root AGENT_STARTING has no content");
    check_json(
        "configured_root AGENT_STARTING content",
        &content,
        r#""Coordinate everything.""#,
    )
    .unwrap();
    check_attr(&root_start, r#""Root coordinator""#, &["agent_description"]);
    check_attr(&root_start, r#""scripted-model""#, &["model"]);
}

#[tokio::test]
async fn test_trace_manager_shared_plugin_across_independent_runners_and_apps() {
    let tp = setup_test_plugin().await;
    let mut cursor = RowCursor::default();

    // Part 1: first_root -> worker, then standalone worker.
    let worker_child = llm_agent(LlmAgentConfig {
        name: "worker".into(),
        model: Some(scripted_llm(vec![final_text("child worker")])),
        ..Default::default()
    });
    let first_root = llm_agent(LlmAgentConfig {
        name: "first_root".into(),
        model: Some(scripted_llm(vec![final_text("first root done")])),
        sub_agents: vec![worker_child],
        ..Default::default()
    });
    let r1 = app_runner("app-1", first_root, vec![tp.plugin.clone()]);
    let (_, errs) = run_turn(&r1, "u1", "s1", "hi", RunConfig::default()).await;
    assert!(errs.is_empty(), "r1.Run: {errs:?}");
    cursor
        .collect(&tp, until_event_type(EVENT_INVOCATION_COMPLETED))
        .await;

    let standalone_worker = llm_agent(LlmAgentConfig {
        name: "worker".into(),
        model: Some(scripted_llm(vec![final_text("standalone worker")])),
        ..Default::default()
    });
    let r2 = app_runner("app-2", standalone_worker, vec![tp.plugin.clone()]);
    let (_, errs) = run_turn(&r2, "u1", "s2", "hi", RunConfig::default()).await;
    assert!(errs.is_empty(), "r2.Run: {errs:?}");
    let r2_rows = cursor
        .collect(&tp, until_event_type(EVENT_INVOCATION_COMPLETED))
        .await;
    for row in &r2_rows {
        check_attr(row, r#""worker""#, &["root_agent_name"]);
    }

    // Part 2: Two apps (planner -> helper and support -> helper) sharing p.
    let app1_helper = llm_agent(LlmAgentConfig {
        name: "helper".into(),
        instruction: "Help with planning.".into(),
        model: Some(scripted_llm(vec![
            final_text("app1 helper turn 1"),
            final_text("app1 helper turn 2"),
        ])),
        ..Default::default()
    });
    let planner = llm_agent(LlmAgentConfig {
        name: "planner".into(),
        model: Some(scripted_llm(vec![transfer_to("helper")])),
        sub_agents: vec![app1_helper],
        ..Default::default()
    });
    let app2_helper = llm_agent(LlmAgentConfig {
        name: "helper".into(),
        instruction: "Help with support.".into(),
        model: Some(scripted_llm(vec![final_text("app2 helper turn 1")])),
        ..Default::default()
    });
    let support = llm_agent(LlmAgentConfig {
        name: "support".into(),
        model: Some(scripted_llm(vec![transfer_to("helper")])),
        sub_agents: vec![app2_helper],
        ..Default::default()
    });
    let app1_runner = app_runner("app-plan", planner, vec![tp.plugin.clone()]);
    let app2_runner = app_runner("app-support", support, vec![tp.plugin.clone()]);

    // App 1 turn 1 transfers to app1Helper.
    let (_, errs) = run_turn(&app1_runner, "u1", "s-plan", "plan 1", RunConfig::default()).await;
    assert!(errs.is_empty(), "app1Runner turn 1: {errs:?}");
    cursor
        .collect(&tp, until_event_type(EVENT_INVOCATION_COMPLETED))
        .await;

    // App 2 turn 1 runs support -> app2Helper.
    let (_, errs) = run_turn(
        &app2_runner,
        "u1",
        "s-support",
        "support 1",
        RunConfig::default(),
    )
    .await;
    assert!(errs.is_empty(), "app2Runner turn 1: {errs:?}");
    cursor
        .collect(&tp, until_event_type(EVENT_INVOCATION_COMPLETED))
        .await;

    // App 1 turn 2 resumes directly at app1Helper; must report root_agent_name="planner"
    // and app1Helper's own instruction.
    let (_, errs) = run_turn(&app1_runner, "u1", "s-plan", "plan 2", RunConfig::default()).await;
    assert!(errs.is_empty(), "app1Runner turn 2: {errs:?}");
    let app1_turn2 = cursor
        .collect(&tp, until_event_type(EVENT_INVOCATION_COMPLETED))
        .await;
    for row in &app1_turn2 {
        check_attr(row, r#""planner""#, &["root_agent_name"]);
    }
    let helper_start = only_row_of_type(&app1_turn2, EVENT_AGENT_STARTING);
    let content = json_column(&helper_start, "content")
        .expect("app1 turn 2 helper AGENT_STARTING has no content");
    check_json(
        "app1 turn 2 helper AGENT_STARTING content",
        &content,
        r#""Help with planning.""#,
    )
    .unwrap();
}

#[tokio::test]
async fn test_trace_manager_peer_sub_agent_transfer_preserves_transferring_agent_span() {
    let tp = setup_test_plugin().await;
    let helper2 = llm_agent(LlmAgentConfig {
        name: "helper2".into(),
        model: Some(scripted_llm(vec![final_text("helper2 done")])),
        ..Default::default()
    });
    let helper1 = llm_agent(LlmAgentConfig {
        name: "helper1".into(),
        model: Some(scripted_llm(vec![transfer_to("helper2")])),
        ..Default::default()
    });
    let planner = llm_agent(LlmAgentConfig {
        name: "planner".into(),
        model: Some(scripted_llm(vec![transfer_to("helper1")])),
        sub_agents: vec![helper1, helper2],
        ..Default::default()
    });
    let r = new_test_runner(planner, vec![tp.plugin.clone()]);
    let (_, errs) = run_turn(&r, "u1", "s1", "go", RunConfig::default()).await;
    assert!(errs.is_empty(), "r.Run: {errs:?}");

    let rows = RowCursor::default()
        .collect(&tp, until_event_type(EVENT_INVOCATION_COMPLETED))
        .await;
    let by_agent = start_by_agent(&rows);
    let span = |name: &str| by_agent.get(name).cloned().unwrap_or_default();
    assert_eq!(
        span("helper1").parent,
        span("planner").span,
        "helper1 AGENT_STARTING parent, want planner span"
    );
    assert_eq!(
        span("helper2").parent,
        span("helper1").span,
        "helper2 AGENT_STARTING parent, want helper1 span"
    );
    for row in rows_of_type(&rows, EVENT_AGENT_COMPLETED) {
        let name = agent_of(&row);
        assert_eq!(
            Some(&place_of(&row)),
            by_agent.get(&name),
            "{name} AGENT_COMPLETED place"
        );
        must_latency(&row);
    }
}

#[tokio::test]
async fn test_runner_failed_model_call_emits_agent_error_and_invocation_error() {
    let tp = setup_test_plugin().await;
    let model_err = "model failure 404";
    let bot = llm_agent(LlmAgentConfig {
        name: "failing-bot".into(),
        model: Some(failing_llm(model_err)),
        ..Default::default()
    });
    let r = new_test_runner(bot, vec![tp.plugin.clone()]);
    let _ = run_turn(&r, "u1", "s1", "hi", RunConfig::default()).await;

    let rows = RowCursor::default()
        .collect(&tp, until_event_type(EVENT_INVOCATION_ERROR))
        .await;
    let agent_error_rows = rows_of_type(&rows, EVENT_AGENT_ERROR);
    assert!(
        !agent_error_rows.is_empty(),
        "expected at least one AGENT_ERROR row, got 0"
    );
    assert_status(&agent_error_rows[0], "ERROR", Some(model_err));
    let inv_error_rows = rows_of_type(&rows, EVENT_INVOCATION_ERROR);
    assert!(
        !inv_error_rows.is_empty(),
        "expected INVOCATION_ERROR row, got 0"
    );
    assert_status(&inv_error_rows[0], "ERROR", Some(model_err));
}

#[tokio::test]
async fn test_runner_tool_error_emits_invocation_completed() {
    let tp = setup_test_plugin().await;
    let err_tool = function_tool(
        "err_tool",
        "tool that returns an error",
        |_ctx, _args| async move {
            Err(gadk_agent::ToolError::Execution(
                "tool execution failed".into(),
            ))
        },
    );
    let m = scripted_llm(vec![
        Ok(gadk_model::LlmResponse {
            content: Some(gadk_genai::Content {
                role: String::new(),
                parts: vec![function_call_part("err_tool", json!({}))],
            }),
            ..Default::default()
        }),
        Ok(gadk_model::LlmResponse {
            content: Some(text_content("model", "I handled the tool error.")),
            ..Default::default()
        }),
    ]);
    let bot = llm_agent(LlmAgentConfig {
        name: "tool-bot".into(),
        model: Some(m),
        tools: vec![err_tool],
        ..Default::default()
    });
    let r = new_test_runner(bot, vec![tp.plugin.clone()]);
    let (_, errs) = run_turn(&r, "u1", "s1", "run tool", RunConfig::default()).await;
    assert!(errs.is_empty(), "r.Run unexpected errors: {errs:?}");

    let rows = RowCursor::default()
        .collect(&tp, until_event_type(EVENT_INVOCATION_COMPLETED))
        .await;
    assert!(
        !rows_of_type(&rows, EVENT_TOOL_ERROR).is_empty(),
        "expected TOOL_ERROR row, got 0; event types: {:?}",
        event_types(&rows)
    );
    let completed = rows_of_type(&rows, EVENT_INVOCATION_COMPLETED);
    assert_status(&completed[0], "OK", None);
}

// parity: adapted: Go cancels the caller's `ctx` from the canceller plugin's BeforeRunCallback;
// a gadk run has no caller context, so the callback cancels the invocation's token (what the
// caller's cancellation reaches) and returns `AgentError::Cancelled` (Go `context.Canceled`).
#[tokio::test]
async fn test_runner_before_run_cancellation_emits_invocation_error() {
    let tp = setup_test_plugin().await;
    // Registered after p, so that INVOCATION_STARTING is logged before the
    // invocation is canceled.
    let canceller = plugin_named(
        "canceller",
        gadk_agent::PluginConfig {
            before_run_callback: Some(Arc::new(|ictx: gadk_agent::InvocationContext| {
                ictx.cancellation_token().cancel();
                Box::pin(async { Err(gadk_agent::AgentError::Cancelled) })
            })),
            ..Default::default()
        },
    );
    let bot = llm_agent(LlmAgentConfig {
        name: "bot".into(),
        model: Some(scripted_llm(vec![Ok(gadk_model::LlmResponse {
            content: Some(text_content("model", "never sent")),
            ..Default::default()
        })])),
        ..Default::default()
    });
    let r = new_test_runner(bot, vec![tp.plugin.clone(), canceller]);
    let (_, errs) = run_turn(&r, "u1", "s1", "hi", RunConfig::default()).await;
    assert!(
        !errs.is_empty(),
        "r.Run yielded no error, want the cancellation of the BeforeRunCallback"
    );

    let rows = RowCursor::default()
        .collect(&tp, until_invocation_end)
        .await;
    assert!(
        rows_of_type(&rows, EVENT_INVOCATION_COMPLETED).is_empty(),
        "got INVOCATION_COMPLETED for a canceled invocation, want INVOCATION_ERROR; event types: {:?}",
        event_types(&rows)
    );
    let inv_row = only_row_of_type(&rows, EVENT_INVOCATION_ERROR);
    assert_status(&inv_row, "ERROR", Some("context canceled"));
    assert_eq!(
        rows_of_type(&rows, EVENT_AGENT_STARTING).len(),
        0,
        "AGENT_STARTING rows: no agent runs after a BeforeRunCallback error"
    );
    assert!(
        agent_error_counts(&rows).is_empty(),
        "AGENT_ERROR rows per agent = {:?}, want none: no agent was running",
        agent_error_counts(&rows)
    );
}

/// A before-model callback failing with `msg`.
fn failing_before_model(msg: &'static str) -> gadk_agent::BeforeModelCallback {
    Arc::new(move |_ctx, req| {
        Box::pin(async move { (req, Err(gadk_agent::AgentError::other(msg))) })
    })
}

/// A before-model callback answering `text`.
fn answering_before_model(text: &'static str) -> gadk_agent::BeforeModelCallback {
    Arc::new(move |_ctx, req| {
        Box::pin(async move {
            (
                req,
                Ok(Some(gadk_model::LlmResponse {
                    content: Some(text_content("model", text)),
                    ..Default::default()
                })),
            )
        })
    })
}

/// A model answering "never sent" once.
fn never_sent() -> Arc<dyn gadk_model::Llm> {
    scripted_llm(vec![Ok(gadk_model::LlmResponse {
        content: Some(text_content("model", "never sent")),
        ..Default::default()
    })])
}

#[tokio::test]
async fn test_runner_before_model_callback_error_emits_agent_error_and_invocation_error() {
    let tp = setup_test_plugin().await;
    let bot = llm_agent(LlmAgentConfig {
        name: "bot".into(),
        model: Some(never_sent()),
        before_model_callbacks: vec![failing_before_model(
            "request rejected by before-model callback",
        )],
        ..Default::default()
    });
    let r = new_test_runner(bot, vec![tp.plugin.clone()]);
    let (_, errs) = run_turn(&r, "u1", "s1", "hi", RunConfig::default()).await;
    assert!(
        !errs.is_empty(),
        "r.Run yielded no error, want the before-model callback error"
    );

    let rows = RowCursor::default()
        .collect(&tp, until_invocation_end)
        .await;
    assert!(
        rows_of_type(&rows, EVENT_INVOCATION_COMPLETED).is_empty(),
        "got INVOCATION_COMPLETED after a before-model callback error, want INVOCATION_ERROR; event types: {:?}",
        event_types(&rows)
    );
    let want_msg = r#"agent "bot" execution failed before completion"#;
    let agent_row = only_row_of_type(&rows, EVENT_AGENT_ERROR);
    assert_eq!(agent_of(&agent_row), "bot", "AGENT_ERROR agent");
    assert_status(&agent_row, "ERROR", Some(want_msg));
    let inv_row = only_row_of_type(&rows, EVENT_INVOCATION_ERROR);
    assert_status(&inv_row, "ERROR", Some(want_msg));
    assert_eq!(
        rows_of_type(&rows, EVENT_AGENT_COMPLETED).len(),
        0,
        "AGENT_COMPLETED rows: the agent did not complete"
    );
}

#[tokio::test]
async fn test_runner_sequential_sibling_shortcut_then_child_fails_emits_error_only_for_failing_child_and_sequence()
 {
    let tp = setup_test_plugin().await;
    let child1 = llm_agent(LlmAgentConfig {
        name: "child1".into(),
        model: Some(never_sent()),
        before_agent_callbacks: vec![answering_before_agent(text_content(
            "model",
            "answered by callback",
        ))],
        ..Default::default()
    });
    let model_err = "model failure 500";
    let child2 = llm_agent(LlmAgentConfig {
        name: "child2".into(),
        model: Some(failing_llm(model_err)),
        ..Default::default()
    });
    let seq = sequential("sequence", vec![child1, child2]);
    let r = new_test_runner(seq, vec![tp.plugin.clone()]);
    let (_, errs) = run_until_error(&r, "hi").await;
    assert_eq!(
        errs.first().map(ToString::to_string).as_deref(),
        Some(model_err),
        "r.Run error"
    );

    let rows = RowCursor::default()
        .collect(&tp, until_invocation_end)
        .await;
    assert!(
        rows_of_type(&rows, EVENT_INVOCATION_COMPLETED).is_empty(),
        "got INVOCATION_COMPLETED after a model failure, want INVOCATION_ERROR; event types: {:?}",
        event_types(&rows)
    );
    let started: Vec<String> = rows_of_type(&rows, EVENT_AGENT_STARTING)
        .iter()
        .map(agent_of)
        .collect();
    assert_eq!(
        started,
        ["sequence", "child1", "child2"],
        "AGENT_STARTING agents"
    );
    assert_eq!(
        agent_error_counts(&rows),
        BTreeMap::from([("child2".to_owned(), 1), ("sequence".to_owned(), 1)]),
        "AGENT_ERROR rows per agent"
    );
    for row in rows_of_type(&rows, EVENT_AGENT_ERROR) {
        assert_status(&row, "ERROR", Some(model_err));
    }
    let inv_row = only_row_of_type(&rows, EVENT_INVOCATION_ERROR);
    assert_status(&inv_row, "ERROR", Some(model_err));
}

#[tokio::test]
async fn test_runner_cached_before_model_callback_response_emits_invocation_completed_ok() {
    let tp = setup_test_plugin().await;
    let bot = llm_agent(LlmAgentConfig {
        name: "bot".into(),
        model: Some(never_sent()),
        before_model_callbacks: vec![answering_before_model("cached response")],
        ..Default::default()
    });
    let r = new_test_runner(bot, vec![tp.plugin.clone()]);
    let (_, errs) = run_until(
        &r,
        "u1",
        "s1",
        "hi",
        RunConfig::default(),
        |item| matches!(item, Ok(ev) if ev.response.content.is_some()),
    )
    .await;
    assert!(errs.is_empty(), "r.Run error: {errs:?}");

    let rows = RowCursor::default()
        .collect(&tp, until_invocation_end)
        .await;
    assert!(
        rows_of_type(&rows, EVENT_INVOCATION_ERROR).is_empty(),
        "got INVOCATION_ERROR after cached response consumed, want INVOCATION_COMPLETED; event types: {:?}",
        event_types(&rows)
    );
    let inv_row = only_row_of_type(&rows, EVENT_INVOCATION_COMPLETED);
    assert_status(&inv_row, "OK", None);
    assert_eq!(
        rows_of_type(&rows, EVENT_AGENT_ERROR).len(),
        0,
        "AGENT_ERROR rows"
    );
}

#[tokio::test]
async fn test_runner_custom_agent_output_event_emits_invocation_completed_ok() {
    let tp = setup_test_plugin().await;
    let custom_bot = custom_agent("custom_bot", vec![], |ctx| {
        let mut ev = new_event(&ctx);
        ev.output = Some(json!({"answer": 42}));
        futures::stream::iter([Ok(ev)]).boxed()
    });
    let r = new_test_runner(custom_bot, vec![tp.plugin.clone()]);
    let (_, errs) = run_until(
        &r,
        "u1",
        "s1",
        "hi",
        RunConfig::default(),
        |item| matches!(item, Ok(ev) if ev.output.is_some()),
    )
    .await;
    assert!(errs.is_empty(), "r.Run error: {errs:?}");

    let rows = RowCursor::default()
        .collect(&tp, until_invocation_end)
        .await;
    assert!(
        rows_of_type(&rows, EVENT_INVOCATION_ERROR).is_empty(),
        "got INVOCATION_ERROR after the output event was consumed, want INVOCATION_COMPLETED; event types: {:?}",
        event_types(&rows)
    );
    let inv_row = only_row_of_type(&rows, EVENT_INVOCATION_COMPLETED);
    assert_status(&inv_row, "OK", None);
    assert_eq!(
        rows_of_type(&rows, EVENT_AGENT_ERROR).len(),
        0,
        "AGENT_ERROR rows"
    );
}

#[tokio::test]
async fn test_runner_custom_run_error() {
    let tp = setup_test_plugin().await;
    let run_err = "custom run failure";
    let custom_bot = custom_agent("custom_bot", vec![], move |_ctx| {
        futures::stream::iter([Err(gadk_agent::AgentError::other(run_err))]).boxed()
    });
    let r = new_test_runner(custom_bot, vec![tp.plugin.clone()]);
    let (_, errs) = run_until_error(&r, "hi").await;
    assert_eq!(
        errs.first().map(ToString::to_string).as_deref(),
        Some(run_err),
        "r.Run error"
    );

    let rows = RowCursor::default()
        .collect(&tp, until_invocation_end)
        .await;
    let agent_err_row = only_row_of_type(&rows, EVENT_AGENT_ERROR);
    assert_status(&agent_err_row, "ERROR", None);
    let inv_row = only_row_of_type(&rows, EVENT_INVOCATION_ERROR);
    assert_status(&inv_row, "ERROR", None);
}

#[tokio::test]
async fn test_runner_prior_plugin_before_model_callback_error_emits_agent_error_and_invocation_error()
 {
    let tp = setup_test_plugin().await;
    let prior_plugin = plugin_named(
        "prior_failing_plugin",
        gadk_agent::PluginConfig {
            before_model_callback: Some(failing_before_model("prior plugin before model failure")),
            ..Default::default()
        },
    );
    let bot = llm_agent(LlmAgentConfig {
        name: "bot".into(),
        model: Some(scripted_llm(vec![Ok(gadk_model::LlmResponse {
            content: Some(text_content("model", "hello")),
            ..Default::default()
        })])),
        ..Default::default()
    });
    let r = new_test_runner(bot, vec![prior_plugin, tp.plugin.clone()]);
    let (_, errs) = run_until_error(&r, "hi").await;
    assert!(
        !errs.is_empty(),
        "r.Run returned nil error, want prior plugin before model error"
    );

    let rows = RowCursor::default()
        .collect(&tp, until_invocation_end)
        .await;
    let agent_err_row = only_row_of_type(&rows, EVENT_AGENT_ERROR);
    assert_status(&agent_err_row, "ERROR", None);
    let inv_row = only_row_of_type(&rows, EVENT_INVOCATION_ERROR);
    assert_status(&inv_row, "ERROR", None);
}

#[tokio::test]
async fn test_runner_after_agent_callback_error() {
    let tp = setup_test_plugin().await;
    let bot = llm_agent(LlmAgentConfig {
        name: "bot".into(),
        model: Some(scripted_llm(vec![Ok(gadk_model::LlmResponse {
            content: Some(text_content("model", "hello")),
            ..Default::default()
        })])),
        after_agent_callbacks: vec![Arc::new(|_ctx| {
            Box::pin(async {
                Err(gadk_agent::AgentError::other(
                    "after agent callback rejected",
                ))
            })
        })],
        ..Default::default()
    });
    let r = new_test_runner(bot, vec![tp.plugin.clone()]);
    let (_, errs) = run_until_error(&r, "hi").await;
    assert!(
        !errs.is_empty(),
        "r.Run returned nil error, want after agent callback error"
    );

    let rows = RowCursor::default()
        .collect(&tp, until_invocation_end)
        .await;
    let inv_row = only_row_of_type(&rows, EVENT_INVOCATION_COMPLETED);
    assert_status(&inv_row, "OK", None);
}

#[tokio::test]
async fn test_runner_sse_streaming_model_call_end_keeps_request_span_and_latency() {
    let gap = Duration::from_millis(20);
    let model_err = "stream broken 503";
    struct Case {
        name: &'static str,
        leading: Vec<gadk_model::LlmResponse>,
        chunks: Vec<&'static str>,
        err: Option<&'static str>,
        // want_end is the type of the row that ends the model call.
        want_end: &'static str,
        want_invocation: &'static str,
        want_agent_errors: BTreeMap<String, usize>,
    }
    let tests = vec![
        Case {
            name: "final response",
            leading: vec![],
            chunks: vec!["Hel", "lo"],
            err: None,
            want_end: EVENT_LLM_RESPONSE,
            want_invocation: EVENT_INVOCATION_COMPLETED,
            want_agent_errors: BTreeMap::new(),
        },
        Case {
            name: "final response after a leading usage-only chunk",
            leading: vec![gadk_model::LlmResponse {
                partial: true,
                usage_metadata: Some(gadk_genai::GenerateContentResponseUsageMetadata {
                    prompt_token_count: 10,
                    ..Default::default()
                }),
                ..Default::default()
            }],
            chunks: vec!["Hel", "lo"],
            err: None,
            want_end: EVENT_LLM_RESPONSE,
            want_invocation: EVENT_INVOCATION_COMPLETED,
            want_agent_errors: BTreeMap::new(),
        },
        Case {
            name: "error after a chunk",
            leading: vec![],
            chunks: vec!["Hel"],
            err: Some(model_err),
            want_end: EVENT_LLM_ERROR,
            want_invocation: EVENT_INVOCATION_ERROR,
            want_agent_errors: BTreeMap::from([("bot".to_owned(), 1)]),
        },
    ];
    for tc in tests {
        let name = tc.name;
        let tp = setup_test_plugin().await;
        let n_chunks = tc.chunks.len();
        let bot = llm_agent(LlmAgentConfig {
            name: "bot".into(),
            model: Some(streaming_llm(tc.leading, tc.chunks, gap, tc.err)),
            ..Default::default()
        });
        let r = new_test_runner(bot, vec![tp.plugin.clone()]);
        let cfg = RunConfig {
            streaming_mode: gadk_agent::StreamingMode::Sse,
            ..Default::default()
        };
        let (events, errs) = run_turn(&r, "u1", "s1", "hi", cfg).await;
        let partials = events.iter().filter(|ev| ev.response.partial).count();
        assert_eq!(
            partials, n_chunks,
            "{name}: r.Run partial events: the model call was not streamed"
        );
        assert_eq!(
            errs.last().map(ToString::to_string).as_deref(),
            tc.err,
            "{name}: r.Run error"
        );

        let rows = RowCursor::default()
            .collect(&tp, until_invocation_end)
            .await;
        let inv_row = only_row_of_type(&rows, tc.want_invocation);
        if tc.err.is_none() {
            assert_status(&inv_row, "OK", None);
        }
        assert_eq!(
            agent_error_counts(&rows),
            tc.want_agent_errors,
            "{name}: AGENT_ERROR rows per agent"
        );
        let request = only_row_of_type(&rows, EVENT_LLM_REQUEST);
        let end = only_row_of_type(&rows, tc.want_end);
        assert_eq!(
            place_of(&end),
            place_of(&request),
            "{name}: {} place, want the place of LLM_REQUEST",
            tc.want_end
        );
        let latency = must_latency(&end);
        let keys: Vec<&str> = latency.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            ["time_to_first_token_ms", "total_ms"],
            "{name}: {} latency_ms keys",
            tc.want_end
        );
        let (ttft, total) = (latency["time_to_first_token_ms"], latency["total_ms"]);
        let want_min = i64::try_from(gap.as_millis()).unwrap();
        assert!(
            ttft >= want_min && total - ttft >= want_min,
            "{name}: {} latency_ms = {latency:?}, want time_to_first_token_ms >= {want_min} (the first chunk with generated content) and total_ms >= time_to_first_token_ms + {want_min} (the end of the stream)",
            tc.want_end
        );
    }
}

#[tokio::test]
async fn test_runner_sub_agent_cached_before_model_callback_response_emits_invocation_completed_ok()
{
    let new_sub = || {
        llm_agent(LlmAgentConfig {
            name: "sub".into(),
            model: Some(never_sent()),
            before_model_callbacks: vec![answering_before_model("cached response")],
            ..Default::default()
        })
    };
    type NewRoot = fn(Arc<dyn gadk_agent::Agent>) -> Arc<dyn gadk_agent::Agent>;
    let tests: Vec<(&str, NewRoot)> = vec![
        // The sequential parent writes no response of its own.
        ("sequential parent", |sub| sequential("parent", vec![sub])),
        ("transfer from an LLM parent", |sub| {
            llm_agent(LlmAgentConfig {
                name: "parent".into(),
                model: Some(scripted_llm(vec![transfer_to("sub")])),
                sub_agents: vec![sub],
                ..Default::default()
            })
        }),
    ];
    for (name, new_root) in tests {
        let tp = setup_test_plugin().await;
        let r = new_test_runner(new_root(new_sub()), vec![tp.plugin.clone()]);
        let mut got_cached = false;
        let (_, errs) = run_until(&r, "u1", "s1", "hi", RunConfig::default(), |item| {
            got_cached =
                matches!(item, Ok(ev) if ev.author == "sub" && ev.response.content.is_some());
            got_cached
        })
        .await;
        assert!(errs.is_empty(), "{name}: r.Run error: {errs:?}");
        assert!(
            got_cached,
            "{name}: r.Run ended without the sub-agent's cached response"
        );

        let rows = RowCursor::default()
            .collect(&tp, until_invocation_end)
            .await;
        require_invocation_completed_ok(&rows);
    }
}

#[tokio::test]
async fn test_runner_shared_same_agent_root() {
    for tree_first in [true, false] {
        let tp = setup_test_plugin().await;
        let worker = llm_agent(LlmAgentConfig {
            name: "worker".into(),
            model: Some(scripted_llm(vec![final_text("worker answer")])),
            ..Default::default()
        });
        let root = llm_agent(LlmAgentConfig {
            name: "root".into(),
            model: Some(scripted_llm(vec![final_text("root answer")])),
            sub_agents: vec![worker.clone()],
            ..Default::default()
        });
        let mut cursor = RowCursor::default();
        // run runs r to its end and checks the root agent of every row.
        let order: [(&Arc<dyn gadk_agent::Agent>, &str); 2] = if tree_first {
            [(&root, "root"), (&worker, "worker")]
        } else {
            [(&worker, "worker"), (&root, "root")]
        };
        for (agent, want_root) in order {
            let r = new_test_runner(agent.clone(), vec![tp.plugin.clone()]);
            let (_, errs) = run_turn(&r, "u1", "s1", "hi", RunConfig::default()).await;
            assert!(errs.is_empty(), "r.Run (root {want_root}): {errs:?}");
            let rows = cursor
                .collect(&tp, until_event_type(EVENT_INVOCATION_COMPLETED))
                .await;
            for row in &rows {
                check_attr(row, &format!("{want_root:?}"), &["root_agent_name"]);
            }
        }
    }
}

#[tokio::test]
async fn test_runner_custom_parent_swallows_short_circuited_child_emits_invocation_completed_ok() {
    type Respond = fn(&mut gadk_session::Event);
    type IsFinal = fn(&gadk_session::Event) -> bool;
    let tests: Vec<(&str, Respond, IsFinal)> = vec![
        (
            "content",
            |ev| ev.response.content = Some(text_content("model", "parent answer")),
            |ev| ev.response.content.is_some(),
        ),
        (
            "output",
            |ev| ev.output = Some(json!({"answer": 42})),
            |ev| ev.output.is_some(),
        ),
    ];
    for (name, respond, is_final) in tests {
        let tp = setup_test_plugin().await;
        let sub = llm_agent(LlmAgentConfig {
            name: "sub".into(),
            model: Some(never_sent()),
            before_agent_callbacks: vec![answering_before_agent(text_content(
                "model",
                "answered by callback",
            ))],
            ..Default::default()
        });
        let run_sub = sub.clone();
        let parent = custom_agent("parent", vec![sub], move |ctx| {
            let sub = run_sub.clone();
            async_stream_of(async move {
                let mut events = sub.run(ctx.clone());
                while events.next().await.is_some() {}
                let mut ev = new_event(&ctx);
                respond(&mut ev);
                vec![Ok(ev)]
            })
        });
        let r = new_test_runner(parent, vec![tp.plugin.clone()]);
        let mut got_final = false;
        let (_, errs) = run_until(&r, "u1", "s1", "hi", RunConfig::default(), |item| {
            got_final = matches!(item, Ok(ev) if ev.author == "parent" && is_final(ev));
            got_final
        })
        .await;
        assert!(errs.is_empty(), "{name}: r.Run error: {errs:?}");
        assert!(
            got_final,
            "{name}: r.Run ended without the final event of the parent"
        );

        let rows = RowCursor::default()
            .collect(&tp, until_invocation_end)
            .await;
        assert_eq!(
            rows_of_type(&rows, EVENT_AGENT_STARTING).len(),
            2,
            "{name}: AGENT_STARTING rows, want 2 (parent and sub)"
        );
        require_invocation_completed_ok(&rows);
    }
}

#[tokio::test]
async fn test_runner_sequential_sibling_shortcut_then_custom_child_fails_blames_only_failing_child()
{
    let tp = setup_test_plugin().await;
    let child1 = llm_agent(LlmAgentConfig {
        name: "child1".into(),
        model: Some(never_sent()),
        before_agent_callbacks: vec![answering_before_agent(text_content(
            "model",
            "answered by callback",
        ))],
        ..Default::default()
    });
    let run_err = "custom child failure";
    let child2 = custom_agent("child2", vec![], move |_ctx| {
        futures::stream::iter([Err(gadk_agent::AgentError::other(run_err))]).boxed()
    });
    let seq = sequential("sequence", vec![child1, child2]);
    let r = new_test_runner(seq, vec![tp.plugin.clone()]);
    let (_, errs) = run_until_error(&r, "hi").await;
    assert_eq!(
        errs.first().map(ToString::to_string).as_deref(),
        Some(run_err),
        "r.Run error"
    );

    let rows = RowCursor::default()
        .collect(&tp, until_invocation_end)
        .await;
    assert!(
        rows_of_type(&rows, EVENT_INVOCATION_COMPLETED).is_empty(),
        "got INVOCATION_COMPLETED after a child failure, want INVOCATION_ERROR; event types: {:?}",
        event_types(&rows)
    );
    assert_eq!(
        agent_error_counts(&rows),
        BTreeMap::from([("child2".to_owned(), 1), ("sequence".to_owned(), 1)]),
        "AGENT_ERROR rows per agent"
    );
    only_row_of_type(&rows, EVENT_INVOCATION_ERROR);
}
