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
//! Port of Go agent/llmagent/llmagent_delegation_test.go (httprr replays). Go's
//! `.events.yaml` session dumps are written only when re-recording a trace; they are not
//! assertions and are not ported.

use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use gadk_agent::{Agent, BeforeModelCallback, Context, Mode, RunConfig, ToolError};
use gadk_engine::llm::LlmAgentConfig;
use gadk_engine::llm::agent as llm_agent;
use gadk_engine::runner::{PluginConfig, RunOptions, Runner, RunnerConfig};
use gadk_engine::testutil::{collect_events, gemini_test_model};
use gadk_engine::tool::{FunctionTool, FunctionToolConfig};
use gadk_engine::workflow::{AgentNode, Node, NodeConfig, START, Workflow, WorkflowOptions, chain};
use gadk_genai::{Content, FunctionCall, FunctionResponse, ROLE_MODEL, ROLE_USER, Schema};
use gadk_model::{Llm, LlmRequest};
use gadk_platform::Platform;
use gadk_session::{CreateRequest, Event, GetRequest, InMemorySessionService, SessionService};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

/// Go delegationModelName.
const DELEGATION_MODEL: &str = "gemini-3.5-flash";
const APP: &str = "delegation_app";
const USER: &str = "delegation_user";
const SESSION: &str = "delegation_session";

macro_rules! fixture {
    ($name:literal) => {
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/testdata/agent/llmagent/testdata/",
            $name,
            ".httprr"
        )
    };
}

fn schema(v: Value) -> Schema {
    serde_json::from_value(v).unwrap()
}

/// Port of Go delegationRunner (the .events.yaml dump is a re-recording artifact, not ported).
struct DelegationRunner {
    runner: Runner,
    svc: Arc<dyn SessionService>,
}
impl DelegationRunner {
    async fn new(root: Arc<dyn Agent>) -> Self {
        let svc: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
        let runner = Runner::new(RunnerConfig {
            app_name: APP.into(),
            agent: root,
            session_service: svc.clone(),
            artifact_service: None,
            memory_service: None,
            plugin_config: PluginConfig::default(),
            auto_create_session: false,
            platform: Platform::default(),
        })
        .unwrap_or_else(|e| panic!("runner.New: {e}"));
        svc.create(CreateRequest {
            app_name: APP.into(),
            user_id: USER.into(),
            session_id: Some(SESSION.into()),
            state: None,
        })
        .await
        .unwrap_or_else(|e| panic!("session.Create: {e}"));
        Self { runner, svc }
    }

    /// Go turn: one plain-text user turn.
    async fn turn(&self, msg: &str) -> Vec<Event> {
        self.turn_content(Content::from_text(msg, ROLE_USER)).await
    }

    /// Go turnContent: one turn from a pre-built Content.
    async fn turn_content(&self, msg: Content) -> Vec<Event> {
        collect_events(self.runner.run(
            USER,
            SESSION,
            Some(msg),
            RunConfig::default(),
            RunOptions::default(),
        ))
        .await
        .unwrap_or_else(|e| panic!("collect events: {e}"))
    }

    /// Go sessionEventsSnapshot: every event persisted in the session.
    async fn session_events(&self) -> Vec<Event> {
        self.svc
            .get(GetRequest {
                app_name: APP.into(),
                user_id: USER.into(),
                session_id: SESSION.into(),
                ..Default::default()
            })
            .await
            .unwrap_or_else(|e| panic!("session.Get: {e}"))
            .events()
    }
}

/// Go collectFCsByName.
fn collect_fcs_by_name(events: &[Event], name: &str) -> Vec<FunctionCall> {
    events
        .iter()
        .filter_map(|e| e.response.content.as_ref())
        .flat_map(|c| c.parts.iter())
        .filter_map(|p| p.function_call.clone())
        .filter(|fc| fc.name == name)
        .collect()
}

/// Go collectFRsByName.
fn collect_frs_by_name(events: &[Event], name: &str) -> Vec<FunctionResponse> {
    events
        .iter()
        .filter_map(|e| e.response.content.as_ref())
        .flat_map(|c| c.parts.iter())
        .filter_map(|p| p.function_response.clone())
        .filter(|fr| fr.name == name)
        .collect()
}

/// Go eventsByAuthor.
fn events_by_author<'a>(events: &'a [Event], name: &str) -> Vec<&'a Event> {
    events.iter().filter(|e| e.author == name).collect()
}

/// Order-preserving dedup (Go uniqueBranches / uniqueScopes).
fn unique(values: impl Iterator<Item = String>) -> Vec<String> {
    let mut seen = HashSet::new();
    values.filter(|v| seen.insert(v.clone())).collect()
}
/// Go uniqueScopes.
fn unique_scopes(events: &[&Event]) -> Vec<String> {
    unique(events.iter().map(|e| e.isolation_scope.clone()))
}
/// Go uniqueBranches.
fn unique_branches(events: &[&Event]) -> Vec<String> {
    unique(events.iter().map(|e| e.branch.clone()))
}

/// Go finalModelTextAuthor.
fn final_model_text_author(events: &[Event]) -> String {
    events
        .iter()
        .rev()
        .find(|e| {
            e.response.content.as_ref().is_some_and(|c| {
                c.role == ROLE_MODEL && c.parts.iter().any(|p| !p.text.is_empty() && !p.thought)
            })
        })
        .map(|e| e.author.clone())
        .unwrap_or_default()
}

/// Go finalModelText.
fn final_model_text(events: &[Event]) -> String {
    for e in events.iter().rev() {
        let Some(c) = e.response.content.as_ref() else {
            continue;
        };
        if c.role != ROLE_MODEL {
            continue;
        }
        let parts: Vec<&str> = c
            .parts
            .iter()
            .filter(|p| !p.text.is_empty() && !p.thought)
            .map(|p| p.text.as_str())
            .collect();
        if !parts.is_empty() {
            return parts.concat();
        }
    }
    String::new()
}

/// Go mentionsAll (case-insensitive).
fn mentions_all(s: &str, needles: &[&str]) -> bool {
    let lo = s.to_lowercase();
    needles.iter().all(|n| lo.contains(&n.to_lowercase()))
}

fn agent_name_arg(fc: &FunctionCall) -> &str {
    fc.args
        .get("agent_name")
        .and_then(Value::as_str)
        .unwrap_or_default()
}

/// Go hasTransferTo.
fn has_transfer_to(events: &[Event], name: &str) -> bool {
    collect_fcs_by_name(events, "transfer_to_agent")
        .iter()
        .any(|fc| agent_name_arg(fc) == name)
}

/// Go transferAuthorsByTarget: distinct authors of transfer_to_agent(target) calls.
fn transfer_authors_by_target(events: &[Event], target: &str) -> Vec<String> {
    unique(
        events
            .iter()
            .filter(|ev| {
                ev.response
                    .content
                    .iter()
                    .flat_map(|c| c.parts.iter())
                    .any(|p| {
                        p.function_call.as_ref().is_some_and(|fc| {
                            fc.name == "transfer_to_agent" && agent_name_arg(fc) == target
                        })
                    })
            })
            .map(|ev| ev.author.clone()),
    )
}

/// Go findPendingConfirmationID.
fn find_pending_confirmation_id(events: &[Event]) -> String {
    let resolved: HashSet<String> = collect_frs_by_name(events, "adk_request_confirmation")
        .into_iter()
        .map(|fr| fr.id)
        .collect();
    collect_fcs_by_name(events, "adk_request_confirmation")
        .into_iter()
        .map(|fc| fc.id)
        .find(|id| !resolved.contains(id))
        .unwrap_or_default()
}

fn is_success_fr(fr: &FunctionResponse) -> bool {
    fr.response.get("result").and_then(Value::as_str) == Some("Task completed.")
}

/// Go successFinishFRs.
fn success_finish_frs(events: &[Event]) -> Vec<FunctionResponse> {
    collect_frs_by_name(events, "finish_task")
        .into_iter()
        .filter(is_success_fr)
        .collect()
}

/// Go hasFunctionCall.
fn has_function_call(contents: &[Content], name: &str) -> bool {
    contents
        .iter()
        .flat_map(|c| c.parts.iter())
        .any(|p| p.function_call.as_ref().is_some_and(|fc| fc.name == name))
}

/// Go hasFunctionResponse.
fn has_function_response(contents: &[Content], name: &str) -> bool {
    contents.iter().flat_map(|c| c.parts.iter()).any(|p| {
        p.function_response
            .as_ref()
            .is_some_and(|fr| fr.name == name)
    })
}

// =============================================================================
// Scenario 1: chat coordinator → 1 task sub-agent → 1 function tool.
// =============================================================================

const DATA_FETCHER_INSTRUCTION: &str = r#"You collect a structured list of items for the coordinator.
You have no tools. When you receive a request, immediately call `finish_task`
with the items field populated from the request. Do not ask follow-up
questions; the request always describes the items you should return."#;

const COORDINATOR_01_INSTRUCTION: &str = r#"You are a coordinator with one sub-agent and one tool.

WORKFLOW:
  1. Always call the data_fetcher sub-agent first with a request
     describing the items the user mentioned.
  2. Once data_fetcher returns its structured `output` (a list of items),
     call the store_report tool exactly once with those items.
  3. After store_report returns, reply with a single concise sentence
     confirming the number stored. Do not call any tool again.

Never ask the user for clarification; the user's first message always
contains everything you need."#;

#[derive(Deserialize, JsonSchema)]
struct StoreReportArgs {
    items: Vec<String>,
}
#[derive(Serialize, JsonSchema)]
struct StoreReportResult {
    stored: usize,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_delegation_01_chat_to_task_to_tool() {
    let model: Arc<dyn Llm> = gemini_test_model(
        DELEGATION_MODEL,
        fixture!("TestDelegation_01_ChatToTaskToTool"),
    )
    .await;

    // Output schema for data_fetcher: {items: []string}.
    let items_schema = schema(json!({
        "type": "OBJECT",
        "properties": {"items": {"type": "ARRAY", "description": "List of fetched items.", "items": {"type": "STRING"}}},
        "required": ["items"]
    }));
    let data_fetcher = llm_agent::new(LlmAgentConfig {
        name: "data_fetcher".into(),
        description: "Fetches a list of items requested by the coordinator.".into(),
        model: Some(model.clone()),
        mode: Mode::Task,
        output_schema: Some(items_schema),
        instruction: DATA_FETCHER_INSTRUCTION.into(),
        ..Default::default()
    })
    .unwrap_or_else(|e| panic!("data_fetcher: {e}"));

    // store_report is a plain function tool the coordinator must call AFTER receiving the
    // data_fetcher result.
    let store_calls = Arc::new(AtomicUsize::new(0));
    let stored_items = Arc::new(Mutex::new(Vec::<String>::new()));
    let store_report = {
        let (calls, stored) = (store_calls.clone(), stored_items.clone());
        FunctionTool::new(
            FunctionToolConfig {
                input_schema: Some(json!({"additionalProperties":false,"properties":{"items":{"items":{"type":"string"},"type":["null","array"]}},"required":["items"],"type":"object"})),
                output_schema: Some(json!({"additionalProperties":false,"properties":{"stored":{"type":"integer"}},"required":["stored"],"type":"object"})),
                ..FunctionToolConfig::new("store_report", "Stores the final list of items.")
            },
            move |_ctx: Context, input: StoreReportArgs| {
                let (calls, stored) = (calls.clone(), stored.clone());
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    let n = input.items.len();
                    *stored.lock().unwrap() = input.items;
                    Ok::<_, ToolError>(StoreReportResult { stored: n })
                }
            },
        )
        .unwrap_or_else(|e| panic!("store_report: {e}"))
    };

    let coordinator = llm_agent::new(LlmAgentConfig {
        name: "coordinator".into(),
        description: "Routes requests to data_fetcher, then stores the result.".into(),
        model: Some(model.clone()),
        mode: Mode::Chat,
        sub_agents: vec![data_fetcher],
        tools: vec![store_report],
        instruction: COORDINATOR_01_INSTRUCTION.into(),
        ..Default::default()
    })
    .unwrap_or_else(|e| panic!("coordinator: {e}"));

    let dr = DelegationRunner::new(coordinator).await;

    // --- Turn 1: fetch and store ---
    let events1 = dr
        .turn("Fetch and store the following items: apple, banana, cherry.")
        .await;
    // (1) Coordinator emits exactly one TaskAgentTool FC for data_fetcher.
    assert_eq!(
        collect_fcs_by_name(&events1, "data_fetcher").len(),
        1,
        "turn 1: data_fetcher FCs"
    );
    // (2) data_fetcher emits exactly one finish_task FC with a successful FR back to it.
    assert_eq!(
        collect_fcs_by_name(&events1, "finish_task").len(),
        1,
        "turn 1: finish_task FCs"
    );
    let finish_frs = collect_frs_by_name(&events1, "finish_task");
    assert_eq!(finish_frs.len(), 1, "turn 1: finish_task FRs");
    assert_eq!(
        finish_frs[0].response.get("result"),
        Some(&json!("Task completed.")),
        "turn 1: finish_task FR result"
    );
    // (3) The synthesized FR back to the coordinator carries the structured items (an object
    //     schema passes the map through, not wrapped under `output`).
    let fetcher_frs = collect_frs_by_name(&events1, "data_fetcher");
    assert_eq!(fetcher_frs.len(), 1, "turn 1: data_fetcher FRs");
    assert!(
        fetcher_frs[0].response.contains_key("items"),
        "turn 1: data_fetcher FR missing `items`: {:?}",
        fetcher_frs[0].response
    );
    // (4) store_report is invoked exactly once with the items.
    assert_eq!(
        store_calls.load(Ordering::SeqCst),
        1,
        "turn 1: store_report invocations"
    );
    assert!(
        !stored_items.lock().unwrap().is_empty(),
        "turn 1: store_report received empty items"
    );
    // (5) data_fetcher's events are scope-isolated from the coordinator's.
    let task_events = events_by_author(&events1, "data_fetcher");
    assert!(
        !task_events.is_empty(),
        "turn 1: no events authored by data_fetcher"
    );
    for ev in task_events {
        assert!(
            !ev.isolation_scope.is_empty(),
            "turn 1: data_fetcher event has empty IsolationScope: {ev:?}"
        );
    }

    // --- Turn 2: a follow-up that requires memory of turn 1 ---
    let events2 = dr.turn("What items did I just ask you to store?").await;
    assert_eq!(
        collect_fcs_by_name(&events2, "data_fetcher").len(),
        0,
        "turn 2: data_fetcher FCs (coordinator should answer from history, not re-dispatch)"
    );
    assert_eq!(
        store_calls.load(Ordering::SeqCst),
        1,
        "turn 2: store_report total invocations (no new tool calls on turn 2)"
    );
    assert_eq!(
        final_model_text_author(&events2),
        "coordinator",
        "turn 2: final model text author"
    );
    let reply = final_model_text(&events2);
    assert!(
        mentions_all(&reply, &["apple", "banana", "cherry"]),
        "turn 2: coordinator's reply {reply:?} should mention all stored items (apple, banana, cherry)"
    );
}

// =============================================================================
// Scenario 2: chat coordinator → two task sub-agents called sequentially in one user turn.
// =============================================================================

const ORDER_COLLECTOR_INSTRUCTION: &str = r#"Extract the order from the request and immediately call
`finish_task` with {item, quantity}. The request always contains
both. Do not ask the user any questions."#;

const PAYMENT_COLLECTOR_INSTRUCTION: &str = r#"Extract credit_card_number and cvv from the request and
immediately call `finish_task` with both fields. The request always
contains both. Do not ask the user any questions."#;

const COORDINATOR_02_INSTRUCTION: &str = r#"You orchestrate the order pipeline in TWO sequential steps:

  1. First call order_collector with a request describing the user's
     order (item + quantity). Wait for its structured `output`.
  2. Then call payment_collector with a request describing the user's
     payment details. Wait for its structured `output`.
  3. Reply with one concise sentence acknowledging both pieces.

Never ask the user follow-up questions; the user's first message
always contains everything needed for both steps."#;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_delegation_02_chat_to_two_task_sequential() {
    let model: Arc<dyn Llm> = gemini_test_model(
        DELEGATION_MODEL,
        fixture!("TestDelegation_02_ChatToTwoTaskSequential"),
    )
    .await;

    let order_schema = schema(json!({
        "type": "OBJECT",
        "properties": {"item": {"type": "STRING"}, "quantity": {"type": "INTEGER"}},
        "required": ["item", "quantity"]
    }));
    let payment_schema = schema(json!({
        "type": "OBJECT",
        "properties": {"credit_card_number": {"type": "STRING"}, "cvv": {"type": "STRING"}},
        "required": ["credit_card_number", "cvv"]
    }));
    let order_collector = llm_agent::new(LlmAgentConfig {
        name: "order_collector".into(),
        description: "Collects the order item and quantity from the request.".into(),
        model: Some(model.clone()),
        mode: Mode::Task,
        output_schema: Some(order_schema),
        instruction: ORDER_COLLECTOR_INSTRUCTION.into(),
        ..Default::default()
    })
    .unwrap_or_else(|e| panic!("order_collector: {e}"));
    let payment_collector = llm_agent::new(LlmAgentConfig {
        name: "payment_collector".into(),
        description: "Collects credit card details from the request.".into(),
        model: Some(model.clone()),
        mode: Mode::Task,
        output_schema: Some(payment_schema),
        instruction: PAYMENT_COLLECTOR_INSTRUCTION.into(),
        ..Default::default()
    })
    .unwrap_or_else(|e| panic!("payment_collector: {e}"));
    let coordinator = llm_agent::new(LlmAgentConfig {
        name: "coordinator".into(),
        description: "Calls order_collector, then payment_collector, then summarises.".into(),
        model: Some(model.clone()),
        mode: Mode::Chat,
        sub_agents: vec![order_collector, payment_collector],
        instruction: COORDINATOR_02_INSTRUCTION.into(),
        ..Default::default()
    })
    .unwrap_or_else(|e| panic!("coordinator: {e}"));

    let dr = DelegationRunner::new(coordinator).await;

    // --- Turn 1: order + payment in one user message ---
    let events1 = dr
        .turn("Order 2 pizzas. Pay with card 4111111111111111, cvv 123.")
        .await;
    assert_eq!(
        collect_fcs_by_name(&events1, "order_collector").len(),
        1,
        "turn 1: order_collector FCs"
    );
    assert_eq!(
        collect_fcs_by_name(&events1, "payment_collector").len(),
        1,
        "turn 1: payment_collector FCs"
    );
    let order_frs = collect_frs_by_name(&events1, "order_collector");
    assert_eq!(order_frs.len(), 1, "turn 1: order_collector FRs");
    assert!(
        order_frs[0].response.contains_key("item"),
        "turn 1: order_collector FR missing `item`: {:?}",
        order_frs[0].response
    );
    let pay_frs = collect_frs_by_name(&events1, "payment_collector");
    assert_eq!(pay_frs.len(), 1, "turn 1: payment_collector FRs");
    assert!(
        pay_frs[0].response.contains_key("credit_card_number"),
        "turn 1: payment_collector FR missing `credit_card_number`: {:?}",
        pay_frs[0].response
    );
    let order_scopes = unique_scopes(&events_by_author(&events1, "order_collector"));
    assert!(
        order_scopes.len() == 1 && !order_scopes[0].is_empty(),
        "turn 1: order_collector scopes = {order_scopes:?}, want exactly 1 non-empty"
    );
    let pay_scopes = unique_scopes(&events_by_author(&events1, "payment_collector"));
    assert!(
        pay_scopes.len() == 1 && !pay_scopes[0].is_empty(),
        "turn 1: payment_collector scopes = {pay_scopes:?}, want exactly 1 non-empty"
    );
    assert_ne!(
        order_scopes[0], pay_scopes[0],
        "turn 1: each task delegation must run in its own isolation scope"
    );

    // --- Turn 2: follow-up requiring memory of both delegations ---
    let events2 = dr.turn("What did I order, and how am I paying?").await;
    assert_eq!(
        collect_fcs_by_name(&events2, "order_collector").len(),
        0,
        "turn 2: order_collector FCs"
    );
    assert_eq!(
        collect_fcs_by_name(&events2, "payment_collector").len(),
        0,
        "turn 2: payment_collector FCs"
    );
    assert_eq!(
        final_model_text_author(&events2),
        "coordinator",
        "turn 2: final model text author"
    );
    let reply = final_model_text(&events2);
    assert!(
        mentions_all(&reply, &["pizza"]),
        "turn 2: coordinator's reply {reply:?} should mention the pizza"
    );
}

// =============================================================================
// Scenario 3: chat coordinator → single_turn sub-agent.
// =============================================================================

const TRANSLATOR_03_INSTRUCTION: &str = r#"You translate one phrase. Read the request, then output ONLY
a JSON object of the form {"translation":"..."}. Output nothing else."#;

const COORDINATOR_03_INSTRUCTION: &str = r#"For any user request asking for a translation, call the
translator sub-agent EXACTLY ONCE with a request describing the
phrase and target language. After the sub-agent returns, reply with
one concise English sentence reporting the translation. Do NOT call
the sub-agent more than once. Do NOT ask the user follow-up questions."#;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_delegation_03_chat_to_single_turn() {
    let model: Arc<dyn Llm> = gemini_test_model(
        DELEGATION_MODEL,
        fixture!("TestDelegation_03_ChatToSingleTurn"),
    )
    .await;

    let translation_schema = schema(json!({
        "type": "OBJECT",
        "properties": {"translation": {"type": "STRING"}},
        "required": ["translation"]
    }));
    let translator = llm_agent::new(LlmAgentConfig {
        name: "translator".into(),
        description: "Translates a single phrase to a target language.".into(),
        model: Some(model.clone()),
        mode: Mode::SingleTurn,
        output_schema: Some(translation_schema),
        instruction: TRANSLATOR_03_INSTRUCTION.into(),
        ..Default::default()
    })
    .unwrap_or_else(|e| panic!("translator: {e}"));
    let coordinator = llm_agent::new(LlmAgentConfig {
        name: "coordinator".into(),
        description: "Calls translator and reports the result.".into(),
        model: Some(model.clone()),
        mode: Mode::Chat,
        sub_agents: vec![translator],
        instruction: COORDINATOR_03_INSTRUCTION.into(),
        ..Default::default()
    })
    .unwrap_or_else(|e| panic!("coordinator: {e}"));

    let dr = DelegationRunner::new(coordinator).await;

    // --- Turn 1 ---
    let events1 = dr.turn("Translate 'hello' to Spanish.").await;
    assert_eq!(
        collect_fcs_by_name(&events1, "finish_task").len(),
        0,
        "turn 1: finish_task FCs (single_turn has no finish_task)"
    );
    assert_eq!(
        collect_fcs_by_name(&events1, "translator").len(),
        1,
        "turn 1: translator FCs"
    );
    let trans_frs = collect_frs_by_name(&events1, "translator");
    assert_eq!(trans_frs.len(), 1, "turn 1: translator FRs");
    assert!(
        !trans_frs[0].response.is_empty(),
        "turn 1: translator FR has empty Response: {:?}",
        trans_frs[0]
    );
    let trans_events = events_by_author(&events1, "translator");
    assert!(
        !trans_events.is_empty(),
        "turn 1: no events authored by translator"
    );
    let coord_branch = events_by_author(&events1, "coordinator")
        .first()
        .map(|e| e.branch.clone())
        .unwrap_or_default();
    assert!(
        trans_events
            .iter()
            .any(|ev| !ev.branch.is_empty() && ev.branch != coord_branch),
        "turn 1: expected translator events in a sub-branch distinct from coordinator branch {coord_branch:?}; got {:?}",
        trans_events
            .iter()
            .map(|e| e.branch.as_str())
            .collect::<Vec<_>>()
    );

    // --- Turn 2 ---
    let events2 = dr.turn("Now translate the same word to French.").await;
    assert_eq!(
        collect_fcs_by_name(&events2, "translator").len(),
        1,
        "turn 2: translator FCs (one new dispatch)"
    );
    assert_eq!(
        collect_frs_by_name(&events2, "translator").len(),
        1,
        "turn 2: translator FRs"
    );
    assert_eq!(
        final_model_text_author(&events2),
        "coordinator",
        "turn 2: final model text author"
    );
}

// =============================================================================
// Scenario 4: chat coordinator → two single_turn sub-agents in parallel.
// =============================================================================

const TRANSLATOR_ES_INSTRUCTION: &str = r#"Translate the given English phrase into Spanish. Reply
with ONLY the Spanish translation, no other words."#;

const TRANSLATOR_FR_INSTRUCTION: &str = r#"Translate the given English phrase into French. Reply
with ONLY the French translation, no other words."#;

const COORDINATOR_04_INSTRUCTION: &str = r#"When the user asks for translations into multiple
languages, call BOTH translator sub-agents in your NEXT response —
prefer putting both function calls in a single response so they run
in parallel. After both return, reply with one short sentence
listing both translations."#;

// Go dr.SkipDump() only suppresses the .events.yaml dump, which is not ported.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_delegation_04_chat_to_two_single_turn_parallel() {
    let model: Arc<dyn Llm> = gemini_test_model(
        DELEGATION_MODEL,
        fixture!("TestDelegation_04_ChatToTwoSingleTurnParallel"),
    )
    .await;

    let translator_es = llm_agent::new(LlmAgentConfig {
        name: "translator_es".into(),
        description: "Translates an English phrase into Spanish.".into(),
        model: Some(model.clone()),
        mode: Mode::SingleTurn,
        instruction: TRANSLATOR_ES_INSTRUCTION.into(),
        ..Default::default()
    })
    .unwrap_or_else(|e| panic!("translator_es: {e}"));
    let translator_fr = llm_agent::new(LlmAgentConfig {
        name: "translator_fr".into(),
        description: "Translates an English phrase into French.".into(),
        model: Some(model.clone()),
        mode: Mode::SingleTurn,
        instruction: TRANSLATOR_FR_INSTRUCTION.into(),
        ..Default::default()
    })
    .unwrap_or_else(|e| panic!("translator_fr: {e}"));
    let coordinator = llm_agent::new(LlmAgentConfig {
        name: "coordinator".into(),
        description: "Calls both translators (in parallel) and reports both.".into(),
        model: Some(model.clone()),
        mode: Mode::Chat,
        sub_agents: vec![translator_es, translator_fr],
        instruction: COORDINATOR_04_INSTRUCTION.into(),
        ..Default::default()
    })
    .unwrap_or_else(|e| panic!("coordinator: {e}"));

    let dr = DelegationRunner::new(coordinator).await;

    let events1 = dr
        .turn("Translate 'good morning' to both Spanish and French.")
        .await;
    for name in ["translator_es", "translator_fr"] {
        assert!(
            !collect_fcs_by_name(&events1, name).is_empty(),
            "turn 1: {name} FCs, want >= 1"
        );
        assert!(
            !collect_frs_by_name(&events1, name).is_empty(),
            "turn 1: {name} FRs, want >= 1"
        );
    }
    let es_branches = unique_branches(&events_by_author(&events1, "translator_es"));
    let fr_branches = unique_branches(&events_by_author(&events1, "translator_fr"));
    assert!(
        es_branches.len() == 1 && !es_branches[0].is_empty(),
        "turn 1: translator_es branches = {es_branches:?}, want exactly 1 non-empty"
    );
    assert!(
        fr_branches.len() == 1 && !fr_branches[0].is_empty(),
        "turn 1: translator_fr branches = {fr_branches:?}, want exactly 1 non-empty"
    );
    assert_ne!(
        es_branches[0], fr_branches[0],
        "turn 1: each single_turn sub-agent must run in its own sub-branch"
    );
}

// =============================================================================
// Scenario 5: chat root → (transfer) chat writer → task editor.
// =============================================================================

const EDITOR_INSTRUCTION: &str = r#"Read the text in the request, produce a polished version
(fix grammar, tighten phrasing), and immediately call `finish_task`
with {"edited":"<polished text>"}. Never ask follow-up questions."#;

const WRITER_INSTRUCTION: &str = r#"For EACH draft you receive (a new draft on every
user turn), call the editor sub-agent ONCE with a request
containing the draft text. After editor returns its structured
`output`, reply with one short sentence presenting the edited
text. Always call editor for any new draft, even if you've edited
similar drafts earlier in the conversation."#;

const ROOT_05_INSTRUCTION: &str = r#"For ANY editing request, immediately call
transfer_to_agent with agent_name="writer". Do not attempt to edit
the draft yourself. Do not produce any final text — let writer
respond to the user."#;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_delegation_05_chat_chained_through_chat_to_task() {
    let model: Arc<dyn Llm> = gemini_test_model(
        DELEGATION_MODEL,
        fixture!("TestDelegation_05_ChatChainedThroughChatToTask"),
    )
    .await;

    let edited_doc_schema = schema(json!({
        "type": "OBJECT",
        "properties": {"edited": {"type": "STRING"}},
        "required": ["edited"]
    }));
    let editor = llm_agent::new(LlmAgentConfig {
        name: "editor".into(),
        description: "Edits a piece of text for clarity.".into(),
        model: Some(model.clone()),
        mode: Mode::Task,
        output_schema: Some(edited_doc_schema),
        instruction: EDITOR_INSTRUCTION.into(),
        ..Default::default()
    })
    .unwrap_or_else(|e| panic!("editor: {e}"));
    let writer = llm_agent::new(LlmAgentConfig {
        name: "writer".into(),
        description: "Polishes a draft by delegating to the editor sub-agent.".into(),
        model: Some(model.clone()),
        mode: Mode::Chat,
        sub_agents: vec![editor],
        instruction: WRITER_INSTRUCTION.into(),
        ..Default::default()
    })
    .unwrap_or_else(|e| panic!("writer: {e}"));
    let root = llm_agent::new(LlmAgentConfig {
        name: "root".into(),
        description: "Routes editing requests to the writer agent.".into(),
        model: Some(model.clone()),
        mode: Mode::Chat,
        sub_agents: vec![writer],
        instruction: ROOT_05_INSTRUCTION.into(),
        ..Default::default()
    })
    .unwrap_or_else(|e| panic!("root: {e}"));

    let dr = DelegationRunner::new(root).await;

    // --- Turn 1 ---
    let events1 = dr
        .turn("Please edit this draft: 'me and him goes to the store yesterdy'.")
        .await;
    let transfer_fcs = collect_fcs_by_name(&events1, "transfer_to_agent");
    assert!(
        !transfer_fcs.is_empty(),
        "turn 1: transfer_to_agent FCs = 0, want >= 1"
    );
    assert!(
        transfer_fcs.iter().any(|fc| agent_name_arg(fc) == "writer"),
        "turn 1: expected a transfer_to_agent call with agent_name=writer; got {transfer_fcs:?}"
    );
    assert_eq!(
        collect_fcs_by_name(&events1, "editor").len(),
        1,
        "turn 1: editor FCs"
    );
    let editor_frs = collect_frs_by_name(&events1, "editor");
    assert_eq!(editor_frs.len(), 1, "turn 1: editor FRs");
    assert!(
        editor_frs[0].response.contains_key("edited"),
        "turn 1: editor FR missing `edited`: {:?}",
        editor_frs[0].response
    );
    assert_eq!(
        final_model_text_author(&events1),
        "writer",
        "turn 1: final model text author (writer must produce the user-facing reply after the transfer)"
    );

    // --- Turn 2 ---
    let events2 = dr
        .turn("Now polish this draft: 'they was eatin lunch quick'.")
        .await;
    assert_eq!(
        collect_fcs_by_name(&events2, "transfer_to_agent").len(),
        0,
        "turn 2: transfer_to_agent FCs; writer should remain active without re-transfer"
    );
    assert_eq!(
        collect_fcs_by_name(&events2, "editor").len(),
        1,
        "turn 2: editor FCs (this turn's stream)"
    );
    let session_events = dr.session_events().await;
    let non_empty: Vec<String> = unique_scopes(&events_by_author(&session_events, "editor"))
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect();
    assert_eq!(
        non_empty.len(),
        2,
        "turn 2: distinct editor isolation_scopes session-wide = {non_empty:?}, want 2 (turn-1 scope + new turn-2 scope)"
    );
    assert_eq!(
        final_model_text_author(&events2),
        "writer",
        "turn 2: final model text author (writer should be the active agent across turns)"
    );
}

// =============================================================================
// Scenario 6: chat coordinator → task sub-agent paused on a confirmation (HITL).
// =============================================================================

const EXECUTOR_INSTRUCTION: &str = r#"You execute an irreversible action.

WORKFLOW (exact order):
  1. FIRST call the `confirm` tool (no arguments). This requires
     human approval and will pause your execution.
  2. AFTER the confirm tool returns a successful result, immediately
     call `finish_task` with {"done": true}.

Never skip step 1. Never ask the user follow-up questions in natural
language — always use the confirm tool."#;

const COORDINATOR_06_INSTRUCTION: &str = r#"For any "perform an action" request, call the executor
sub-agent EXACTLY ONCE with a request describing what to do. Once it
returns its structured `output`, reply with one short sentence
confirming completion."#;

#[derive(Deserialize, JsonSchema)]
struct ConfirmInput {}
#[derive(Serialize, JsonSchema)]
struct ConfirmOutput {
    result: String,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_delegation_06_chat_to_task_hitl_resume() {
    let model: Arc<dyn Llm> = gemini_test_model(
        DELEGATION_MODEL,
        fixture!("TestDelegation_06_ChatToTaskHITLResume"),
    )
    .await;

    let confirm_calls = Arc::new(AtomicUsize::new(0));
    let confirm_tool = {
        let calls = confirm_calls.clone();
        FunctionTool::new(
            FunctionToolConfig {
                input_schema: Some(json!({"additionalProperties":false,"type":"object"})),
                output_schema: Some(json!({"additionalProperties":false,"properties":{"result":{"type":"string"}},"required":["result"],"type":"object"})),
                require_confirmation: true,
                ..FunctionToolConfig::new(
                    "confirm",
                    "Confirms the user wants to proceed with the irreversible action.",
                )
            },
            move |_ctx: Context, _in: ConfirmInput| {
                let calls = calls.clone();
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok::<_, ToolError>(ConfirmOutput {
                        result: "Approved. Proceeding.".into(),
                    })
                }
            },
        )
        .unwrap_or_else(|e| panic!("confirm tool: {e}"))
    };

    let result_schema = schema(json!({
        "type": "OBJECT",
        "properties": {"done": {"type": "BOOLEAN"}},
        "required": ["done"]
    }));
    let executor = llm_agent::new(LlmAgentConfig {
        name: "executor".into(),
        description: "Executes an irreversible action after the user confirms.".into(),
        model: Some(model.clone()),
        mode: Mode::Task,
        output_schema: Some(result_schema),
        tools: vec![confirm_tool],
        instruction: EXECUTOR_INSTRUCTION.into(),
        ..Default::default()
    })
    .unwrap_or_else(|e| panic!("executor: {e}"));
    let coordinator = llm_agent::new(LlmAgentConfig {
        name: "coordinator".into(),
        description: "Routes the action to the executor.".into(),
        model: Some(model.clone()),
        mode: Mode::Chat,
        sub_agents: vec![executor],
        instruction: COORDINATOR_06_INSTRUCTION.into(),
        ..Default::default()
    })
    .unwrap_or_else(|e| panic!("coordinator: {e}"));

    let dr = DelegationRunner::new(coordinator).await;

    // --- Turn 1: the executor pauses on the confirmation ---
    let events1 = dr.turn("Please perform the irreversible action.").await;
    assert_eq!(
        collect_fcs_by_name(&events1, "executor").len(),
        1,
        "turn 1: executor FCs"
    );
    assert_eq!(
        collect_frs_by_name(&events1, "executor").len(),
        0,
        "turn 1: executor FRs (delegation must remain unresolved across the HITL pause)"
    );
    let pending_id = find_pending_confirmation_id(&events1);
    assert!(
        !pending_id.is_empty(),
        "turn 1: no pending adk_request_confirmation FC found; events={events1:?}"
    );
    assert_eq!(
        confirm_calls.load(Ordering::SeqCst),
        0,
        "turn 1: confirm handler invocations"
    );

    // --- Turn 2: the user approves ---
    let mut response = Map::new();
    response.insert("confirmed".into(), json!(true));
    let events2 = dr
        .turn_content(Content::from_function_response(
            FunctionResponse {
                id: pending_id,
                name: "adk_request_confirmation".into(),
                response,
                ..Default::default()
            },
            ROLE_USER,
        ))
        .await;
    assert_eq!(
        confirm_calls.load(Ordering::SeqCst),
        1,
        "turn 2: confirm handler invocations (must actually run after the user approves)"
    );
    assert_eq!(
        success_finish_frs(&events2).len(),
        1,
        "turn 2: finish_task success FRs"
    );
    let executor_frs = collect_frs_by_name(&events2, "executor");
    assert_eq!(executor_frs.len(), 1, "turn 2: executor FRs");
    assert_eq!(
        executor_frs[0].response.get("done"),
        Some(&json!(true)),
        "turn 2: executor FR done; resp={:?}",
        executor_frs[0].response
    );

    // --- Turn 3: follow-up answered from history ---
    let events3 = dr.turn("Did the action complete successfully?").await;
    assert_eq!(
        collect_fcs_by_name(&events3, "executor").len(),
        0,
        "turn 3: executor FCs (coordinator should answer from history)"
    );
    assert_eq!(
        final_model_text_author(&events3),
        "coordinator",
        "turn 3: final model text author"
    );
    assert_eq!(
        confirm_calls.load(Ordering::SeqCst),
        1,
        "turn 3: confirm handler invocations (no re-resume)"
    );
}

// =============================================================================
// Scenario 7: task sub-agent's finish_task validation error drives a retry.
// =============================================================================

const SOLVER_INSTRUCTION: &str = r#"You answer one arithmetic question. On your FIRST
finish_task call, intentionally pass a wrong field name ("wrong_field"
instead of "answer") so the validation fails. On your SECOND call,
correct it and pass {"answer": <integer>}. Never give up; the
validation error message will tell you what to fix."#;

const COORDINATOR_07_INSTRUCTION: &str = r#"For any arithmetic question, call the solver sub-agent
EXACTLY ONCE with a request restating the question. Reply with the
answer in one short sentence. Do NOT call solver more than once."#;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_delegation_07_chat_to_task_validation_retry() {
    let model: Arc<dyn Llm> = gemini_test_model(
        DELEGATION_MODEL,
        fixture!("TestDelegation_07_ChatToTaskValidationRetry"),
    )
    .await;

    let answer_schema = schema(json!({
        "type": "OBJECT",
        "properties": {"answer": {"type": "INTEGER", "description": "The numeric answer."}},
        "required": ["answer"]
    }));
    let solver = llm_agent::new(LlmAgentConfig {
        name: "solver".into(),
        description: "Returns the integer answer to a simple arithmetic question.".into(),
        model: Some(model.clone()),
        mode: Mode::Task,
        output_schema: Some(answer_schema),
        instruction: SOLVER_INSTRUCTION.into(),
        ..Default::default()
    })
    .unwrap_or_else(|e| panic!("solver: {e}"));
    let coordinator = llm_agent::new(LlmAgentConfig {
        name: "coordinator".into(),
        description: "Delegates to solver and reports the answer.".into(),
        model: Some(model.clone()),
        mode: Mode::Chat,
        sub_agents: vec![solver],
        instruction: COORDINATOR_07_INSTRUCTION.into(),
        ..Default::default()
    })
    .unwrap_or_else(|e| panic!("coordinator: {e}"));

    let dr = DelegationRunner::new(coordinator).await;

    // --- Turn 1 ---
    let events1 = dr.turn("What is 21 plus 21?").await;
    assert_eq!(
        collect_fcs_by_name(&events1, "solver").len(),
        1,
        "turn 1: solver FCs"
    );
    let finish_fcs = collect_fcs_by_name(&events1, "finish_task");
    assert!(
        finish_fcs.len() >= 2,
        "turn 1: finish_task FCs = {}, want >= 2 (one validation failure + one success)",
        finish_fcs.len()
    );
    let finish_frs = collect_frs_by_name(&events1, "finish_task");
    let err_frs = finish_frs
        .iter()
        .filter(|fr| fr.response.contains_key("error"))
        .count();
    let success_frs = finish_frs
        .iter()
        .filter(|fr| !fr.response.contains_key("error") && is_success_fr(fr))
        .count();
    assert!(
        err_frs >= 1,
        "turn 1: finish_task error FRs = {err_frs}, want >= 1"
    );
    assert_eq!(
        success_frs, 1,
        "turn 1: finish_task success FRs, want exactly 1"
    );
    let solver_frs = collect_frs_by_name(&events1, "solver");
    assert_eq!(solver_frs.len(), 1, "turn 1: solver FRs");
    assert!(
        solver_frs[0].response.contains_key("answer"),
        "turn 1: solver FR missing `answer`: {:?}",
        solver_frs[0].response
    );

    // --- Turn 2 ---
    let events2 = dr
        .turn("What was the answer to the previous question?")
        .await;
    assert_eq!(
        final_model_text_author(&events2),
        "coordinator",
        "turn 2: final model text author"
    );
    let reply = final_model_text(&events2);
    assert!(
        reply.contains("42"),
        "turn 2: coordinator's reply {reply:?} should mention 42"
    );
}

// =============================================================================
// Scenario 8: chat coordinator → task sub-agent, then a plain function tool.
// =============================================================================

const EXTRACTOR_INSTRUCTION: &str = r#"Read the request, extract the list of items mentioned, and
immediately call `finish_task` with {"items":[...]}."#;

const COORDINATOR_08_INSTRUCTION: &str = r#"Two-step workflow:
  1. Call extractor with a request describing the items the user
     mentioned. Wait for its structured `output` containing the items list.
  2. Call summarize EXACTLY ONCE, passing the items from step 1.
  3. Reply with one short sentence repeating the summary."#;

#[derive(Deserialize, JsonSchema)]
struct SummarizeArgs {
    items: Vec<String>,
}
#[derive(Serialize, JsonSchema)]
struct SummarizeResult {
    summary: String,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_delegation_08_chat_to_task_plus_function_tool() {
    let model: Arc<dyn Llm> = gemini_test_model(
        DELEGATION_MODEL,
        fixture!("TestDelegation_08_ChatToTaskPlusFunctionTool"),
    )
    .await;

    let extractor_schema = schema(json!({
        "type": "OBJECT",
        "properties": {"items": {"type": "ARRAY", "items": {"type": "STRING"}}},
        "required": ["items"]
    }));
    let extractor = llm_agent::new(LlmAgentConfig {
        name: "extractor".into(),
        description: "Extracts a list of items from a request.".into(),
        model: Some(model.clone()),
        mode: Mode::Task,
        output_schema: Some(extractor_schema),
        instruction: EXTRACTOR_INSTRUCTION.into(),
        ..Default::default()
    })
    .unwrap_or_else(|e| panic!("extractor: {e}"));

    let summarize_calls = Arc::new(AtomicUsize::new(0));
    let summarize = {
        let calls = summarize_calls.clone();
        FunctionTool::new(
            FunctionToolConfig {
                input_schema: Some(json!({"additionalProperties":false,"properties":{"items":{"items":{"type":"string"},"type":["null","array"]}},"required":["items"],"type":"object"})),
                output_schema: Some(json!({"additionalProperties":false,"properties":{"summary":{"type":"string"}},"required":["summary"],"type":"object"})),
                ..FunctionToolConfig::new(
                    "summarize",
                    "Summarises a list of items into a one-line string.",
                )
            },
            move |_ctx: Context, input: SummarizeArgs| {
                let calls = calls.clone();
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok::<_, ToolError>(SummarizeResult {
                        summary: format!("items: {}", input.items.join(", ")),
                    })
                }
            },
        )
        .unwrap_or_else(|e| panic!("summarize: {e}"))
    };

    let coordinator = llm_agent::new(LlmAgentConfig {
        name: "coordinator".into(),
        description: "First calls extractor, then summarize.".into(),
        model: Some(model.clone()),
        mode: Mode::Chat,
        sub_agents: vec![extractor],
        tools: vec![summarize],
        instruction: COORDINATOR_08_INSTRUCTION.into(),
        ..Default::default()
    })
    .unwrap_or_else(|e| panic!("coordinator: {e}"));

    let dr = DelegationRunner::new(coordinator).await;

    // --- Turn 1 ---
    let events1 = dr.turn("Extract and summarise: apples, bread, eggs.").await;
    assert_eq!(
        collect_fcs_by_name(&events1, "extractor").len(),
        1,
        "turn 1: extractor FCs"
    );
    let ex_frs = collect_frs_by_name(&events1, "extractor");
    assert_eq!(ex_frs.len(), 1, "turn 1: extractor FRs");
    assert!(
        ex_frs[0].response.contains_key("items"),
        "turn 1: extractor FR missing `items`: {:?}",
        ex_frs[0].response
    );
    assert_eq!(
        collect_fcs_by_name(&events1, "summarize").len(),
        1,
        "turn 1: summarize FCs"
    );
    let sum_frs = collect_frs_by_name(&events1, "summarize");
    assert_eq!(sum_frs.len(), 1, "turn 1: summarize FRs");
    assert!(
        sum_frs[0].response.contains_key("summary"),
        "turn 1: summarize FR has no `summary` key (function tools return their result directly, not under `output`); got {:?}",
        sum_frs[0].response
    );
    assert_eq!(
        summarize_calls.load(Ordering::SeqCst),
        1,
        "turn 1: summarize handler invocations"
    );

    // --- Turn 2 ---
    let events2 = dr.turn("How many items did I ask you to summarise?").await;
    assert_eq!(
        collect_fcs_by_name(&events2, "extractor").len(),
        0,
        "turn 2: extractor FCs (no re-dispatch)"
    );
    assert_eq!(
        summarize_calls.load(Ordering::SeqCst),
        1,
        "turn 2: summarize handler total invocations (no new tool calls)"
    );
    assert_eq!(
        final_model_text_author(&events2),
        "coordinator",
        "turn 2: final model text author"
    );
    let reply = final_model_text(&events2);
    assert!(
        reply.contains('3') || reply.to_lowercase().contains("three"),
        "turn 2: coordinator's reply {reply:?} should mention 3 (or 'three')"
    );
}

// =============================================================================
// Scenario 9: chat root → peer transfers between chat siblings.
// =============================================================================

const BILLING_INSTRUCTION: &str = r#"You are the billing specialist. Answer billing-related
questions concisely. If the user asks about anything OTHER than
billing (e.g. technical support), immediately call transfer_to_agent
with agent_name="support" — do not attempt to answer yourself."#;

const SUPPORT_INSTRUCTION: &str = r#"You are the technical support specialist. Answer
technical questions concisely. If the user asks about anything OTHER
than tech support (e.g. billing), immediately call transfer_to_agent
with agent_name="billing"."#;

const ROOT_09_INSTRUCTION: &str = r#"Look at the user's first message and immediately call
transfer_to_agent with agent_name set to "billing" (for billing
questions) or "support" (for technical questions). Never answer
yourself; always transfer."#;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_delegation_09_chat_peer_transfer_across_siblings() {
    let model: Arc<dyn Llm> = gemini_test_model(
        DELEGATION_MODEL,
        fixture!("TestDelegation_09_ChatPeerTransferAcrossSiblings"),
    )
    .await;

    let billing = llm_agent::new(LlmAgentConfig {
        name: "billing".into(),
        description: "Handles billing questions.".into(),
        model: Some(model.clone()),
        mode: Mode::Chat,
        instruction: BILLING_INSTRUCTION.into(),
        ..Default::default()
    })
    .unwrap_or_else(|e| panic!("billing: {e}"));
    let support = llm_agent::new(LlmAgentConfig {
        name: "support".into(),
        description: "Handles technical support questions.".into(),
        model: Some(model.clone()),
        mode: Mode::Chat,
        instruction: SUPPORT_INSTRUCTION.into(),
        ..Default::default()
    })
    .unwrap_or_else(|e| panic!("support: {e}"));
    let root = llm_agent::new(LlmAgentConfig {
        name: "root".into(),
        description: "Routes the user to the right specialist.".into(),
        model: Some(model.clone()),
        mode: Mode::Chat,
        sub_agents: vec![billing, support],
        instruction: ROOT_09_INSTRUCTION.into(),
        ..Default::default()
    })
    .unwrap_or_else(|e| panic!("root: {e}"));

    let dr = DelegationRunner::new(root).await;

    // --- Turn 1: root transfers to billing ---
    let events1 = dr.turn("I have a question about my invoice.").await;
    assert!(
        has_transfer_to(&events1, "billing"),
        "turn 1: expected transfer_to_agent(agent_name=billing); events={events1:?}"
    );
    assert_eq!(
        final_model_text_author(&events1),
        "billing",
        "turn 1 final author (inline-forwarded)"
    );

    // --- Turn 2: billing peer-transfers to support ---
    let events2 = dr.turn("Actually I need help resetting my password.").await;
    let authors = transfer_authors_by_target(&events2, "support");
    assert!(
        !authors.is_empty(),
        "turn 2: expected at least one transfer_to_agent(agent_name=support); events={events2:?}"
    );
    assert!(
        authors.iter().any(|a| a == "billing"),
        "turn 2: peer transfer to support should be authored by `billing` (the last-active agent is re-picked on resume); got authors={authors:?}"
    );
    assert_eq!(
        final_model_text_author(&events2),
        "support",
        "turn 2 final author"
    );

    // --- Turn 3: support peer-transfers back to billing ---
    let events3 = dr
        .turn("Actually, back to my invoice — what's the latest charge?")
        .await;
    let authors = transfer_authors_by_target(&events3, "billing");
    assert!(
        !authors.is_empty(),
        "turn 3: expected at least one transfer_to_agent(agent_name=billing); events={events3:?}"
    );
    assert!(
        authors.iter().any(|a| a == "support"),
        "turn 3: peer transfer back to billing should be authored by `support` (last-active agent from turn 2); got authors={authors:?}"
    );
    assert_eq!(
        final_model_text_author(&events3),
        "billing",
        "turn 3 final author"
    );
}

// =============================================================================
// Scenario 10: a task-mode agent cannot be a static graph node (no model call).
// =============================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_delegation_10_task_agent_rejected_as_static_graph_node() {
    let make_node = |name: &str, mode: Mode| -> Arc<dyn Node> {
        let a = llm_agent::new(LlmAgentConfig {
            name: name.into(),
            mode,
            ..Default::default()
        })
        .unwrap_or_else(|e| panic!("llmagent.New({name:?}, {mode:?}): {e}"));
        AgentNode::new(a, NodeConfig::default()).unwrap_or_else(|e| panic!("NewAgentNode: {e}"))
    };

    // task_mode_rejected
    let task_node = make_node("doer", Mode::Task);
    let err = match Workflow::new(
        "wf",
        chain(&[START.clone(), task_node]),
        WorkflowOptions::default(),
    ) {
        Ok(_) => panic!("workflow.New accepted a task-mode AgentNode; want an error"),
        Err(e) => e,
    };
    assert!(
        err.to_string().contains("task"),
        "error message should mention task mode; got: {err}"
    );

    // chat_mode_accepted / single_turn_mode_accepted
    for (subtest, name, mode) in [
        ("chat_mode_accepted", "chatter", Mode::Chat),
        ("single_turn_mode_accepted", "oneshot", Mode::SingleTurn),
    ] {
        let node = make_node(name, mode);
        if let Err(e) = Workflow::new(
            "wf",
            chain(&[START.clone(), node]),
            WorkflowOptions::default(),
        ) {
            panic!("{subtest}: workflow.New rejected a {mode:?}-mode AgentNode: {e}");
        }
    }
}

// =============================================================================
// Scenario 11: chat coordinator → single_turn sub-agent chaining two tools.
// =============================================================================

const SUB_11_INSTRUCTION: &str = r#"Call tool_one to obtain a token, then call tool_two with
that exact token, then report the result string. Call each tool at most once."#;

const COORDINATOR_11_INSTRUCTION: &str = r#"Call the sub agent exactly once with a brief description
of what to do, then report its answer in one sentence."#;

#[derive(Deserialize, JsonSchema)]
struct NoArgs {}
#[derive(Serialize, Deserialize, JsonSchema)]
struct Token {
    token: String,
}
#[derive(Serialize, JsonSchema)]
struct Echo {
    result: String,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_delegation_11_chat_to_single_turn_chained_tools() {
    let model: Arc<dyn Llm> = gemini_test_model(
        DELEGATION_MODEL,
        fixture!("TestDelegation_11_ChatToSingleTurnChainedTools"),
    )
    .await;

    let t1 = FunctionTool::new(
        FunctionToolConfig {
            input_schema: Some(json!({"additionalProperties":false,"type":"object"})),
            output_schema: Some(json!({"additionalProperties":false,"properties":{"token":{"type":"string"}},"required":["token"],"type":"object"})),
            ..FunctionToolConfig::new("tool_one", "Returns a fixed token string. Call this first.")
        },
        |_ctx: Context, _in: NoArgs| async {
            Ok::<_, ToolError>(Token {
                token: "alpha-42".into(),
            })
        },
    )
    .unwrap_or_else(|e| panic!("tool_one: {e}"));
    let t2 = FunctionTool::new(
        FunctionToolConfig {
            input_schema: Some(json!({"additionalProperties":false,"properties":{"token":{"type":"string"}},"required":["token"],"type":"object"})),
            output_schema: Some(json!({"additionalProperties":false,"properties":{"result":{"type":"string"}},"required":["result"],"type":"object"})),
            ..FunctionToolConfig::new("tool_two", "Echoes the token returned by tool_one.")
        },
        |_ctx: Context, input: Token| async move {
            Ok::<_, ToolError>(Echo {
                result: format!("got {}", input.token),
            })
        },
    )
    .unwrap_or_else(|e| panic!("tool_two: {e}"));

    // Captures each sub-agent model round's request contents.
    let requests: Arc<Mutex<Vec<Vec<Content>>>> = Arc::default();
    let capture: BeforeModelCallback = {
        let requests = requests.clone();
        Arc::new(move |_ctx: Context, req: LlmRequest| {
            requests.lock().unwrap().push(req.contents.clone());
            Box::pin(async move { (req, Ok(None)) })
        })
    };

    let sub = llm_agent::new(LlmAgentConfig {
        name: "sub".into(),
        description: "Chains tool_one then tool_two.".into(),
        model: Some(model.clone()),
        mode: Mode::SingleTurn,
        tools: vec![t1, t2],
        instruction: SUB_11_INSTRUCTION.into(),
        before_model_callbacks: vec![capture],
        ..Default::default()
    })
    .unwrap_or_else(|e| panic!("sub: {e}"));
    let coordinator = llm_agent::new(LlmAgentConfig {
        name: "coordinator".into(),
        description: "Delegates to sub and reports the answer.".into(),
        model: Some(model.clone()),
        mode: Mode::Chat,
        sub_agents: vec![sub],
        instruction: COORDINATOR_11_INSTRUCTION.into(),
        ..Default::default()
    })
    .unwrap_or_else(|e| panic!("coordinator: {e}"));

    let events = DelegationRunner::new(coordinator)
        .await
        .turn("Run the chained tools.")
        .await;

    let requests = requests.lock().unwrap();
    let n = requests.len();
    assert!(
        (1..=8).contains(&n),
        "sub-agent model rounds = {n} (want 1..8); requests: {requests:?}"
    );
    let n = collect_fcs_by_name(&events, "tool_one").len();
    assert!((1..=3).contains(&n), "tool_one FCs = {n}, want 1..3");
    assert!(
        !collect_fcs_by_name(&events, "tool_two").is_empty(),
        "tool_two never called; model could not proceed past tool_one"
    );
    assert!(
        requests
            .iter()
            .any(|c| has_function_call(c, "tool_one") && has_function_response(c, "tool_one")),
        "no captured round contained both tool_one FC and FR; tool history did not survive across rounds. requests: {requests:?}"
    );
}
