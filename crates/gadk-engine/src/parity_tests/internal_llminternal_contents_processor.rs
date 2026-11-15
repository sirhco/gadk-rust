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

//! Port of ADK-Go `internal/llminternal/contents_processor_test.go`, including the
//! package-shared test helpers it defines (`testModel`, `fakeSession`, `wantWithContinuation`).

use std::any::Any;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use gadk_agent::{
    Agent, AgentConfig, AgentError, ContextBuilder, InvocationContext, Mode,
    REQUEST_CONFIRMATION_FUNCTION_CALL_NAME, new_agent,
};
use gadk_genai::{
    Blob, Content, FunctionCall, FunctionResponse, Part, ROLE_MODEL, ROLE_USER, Transcription,
};
use gadk_model::{Llm, LlmRequest, LlmResponse};
use gadk_session::{Event, Session, SessionError, State, ToolConfirmation, zero_time};
use pretty_assertions::assert_eq;
use serde_json::{Map, Value, json};

use crate::llm::flow::Flow;
use crate::llm::processors::contents::{
    SINGLE_TURN_NUDGE, contents_request_processor, convert_foreign_event,
};
use crate::llm::processors::go_json_marshal;
use crate::llm::{IncludeContents, LlmAgentConfig};
use crate::testutil::MockModel;

/// Go contents_processor.go:109 continuation text (used by the Go test helper wantWithContinuation).
pub(super) const CONTINUATION: &str = "Continue processing previous requests as instructed. Exit or provide a summary if no more outputs are needed.";

/// Port of Go `testModel`: an `LLM` the request processors never call.
pub(super) fn test_model() -> Arc<dyn Llm> {
    MockModel::new(vec![])
}

/// Port of Go `fakeSession`: a session whose events are fixed by the test.
pub(super) struct FakeSession {
    events: Vec<Event>,
    state: FakeState,
}

impl FakeSession {
    pub(super) fn shared(events: Vec<Event>) -> Arc<dyn Session> {
        Arc::new(Self {
            events,
            state: FakeState::default(),
        })
    }
}

/// Go's `fakeSession.State()` returns nil; Rust needs a `&dyn State`, so an empty map stands in.
#[derive(Default)]
pub(super) struct FakeState(Mutex<Map<String, Value>>);

impl State for FakeState {
    fn get(&self, key: &str) -> Result<Value, SessionError> {
        self.0
            .lock()
            .unwrap()
            .get(key)
            .cloned()
            .ok_or(SessionError::StateKeyNotExist)
    }
    fn set(&self, key: &str, value: Value) -> Result<(), SessionError> {
        self.0.lock().unwrap().insert(key.to_owned(), value);
        Ok(())
    }
    fn all(&self) -> Vec<(String, Value)> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }
}

impl Session for FakeSession {
    fn id(&self) -> &str {
        "test_session"
    }
    fn app_name(&self) -> &str {
        "test_app"
    }
    fn user_id(&self) -> &str {
        "test_user"
    }
    fn state(&self) -> &dyn State {
        &self.state
    }
    fn events(&self) -> Vec<Event> {
        self.events.clone()
    }
    fn last_update_time(&self) -> DateTime<Utc> {
        zero_time()
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Port of `utils.Must(llmagent.New(cfg))` with `Model: &testModel{}`.
pub(super) fn llm_agent(cfg: LlmAgentConfig) -> Arc<dyn Agent> {
    crate::llm::agent::new(LlmAgentConfig {
        model: Some(test_model()),
        ..cfg
    })
    .unwrap()
}

/// `icontext.InvocationContextParams{Agent, Session: &fakeSession{events}}`; callers add
/// `.branch(..)`, `.isolation_scope(..)` or `.user_content(..)` where Go sets those params.
pub(super) fn ctx_builder(agent: &Arc<dyn Agent>, events: Vec<Event>) -> ContextBuilder {
    InvocationContext::builder()
        .session(FakeSession::shared(events))
        .agent(agent.clone())
}

/// Go's loop over `llminternal.ContentsRequestProcessor(ctx, req, &llminternal.Flow{})`:
/// fails on any yielded event, returns `req.Contents`.
pub(super) async fn run_contents(ctx: &InvocationContext) -> Result<Vec<Content>, AgentError> {
    let mut req = LlmRequest::default();
    let events = contents_request_processor(ctx, &mut req, &Flow::default()).await?;
    assert!(
        events.is_empty(),
        "ContentsRequestProcessor generated an unexpected event"
    );
    Ok(req.contents)
}

/// An event authored by `author` carrying `content` (Go `&session.Event{Author, LLMResponse{Content}}`).
pub(super) fn ev(author: &str, content: Content) -> Event {
    Event {
        author: author.into(),
        response: LlmResponse {
            content: Some(content),
            ..Default::default()
        },
        ..Default::default()
    }
}

pub(super) fn text(t: &str, role: &str) -> Content {
    Content::from_text(t, role)
}

/// Port of Go `wantWithContinuation`.
pub(super) fn with_continuation(mut want: Vec<Content>) -> Vec<Content> {
    if want.last().is_some_and(|last| last.role != ROLE_USER) {
        want.push(Content::from_text(CONTINUATION, ROLE_USER));
    }
    want
}

/// Go `genai.NewContentFromFunctionCall(name, nil, role)`.
fn fc(name: &str, role: &str) -> Content {
    Content::from_function_call(
        FunctionCall {
            name: name.into(),
            ..Default::default()
        },
        role,
    )
}

/// Go `genai.NewContentFromFunctionResponse(name, nil, role)`.
fn fr(name: &str, role: &str) -> Content {
    Content::from_function_response(
        FunctionResponse {
            name: name.into(),
            ..Default::default()
        },
        role,
    )
}

/// The "For context:" rewrite of a foreign event (Go convertForeignEvent output).
fn for_context(line: &str) -> Content {
    Content::from_parts(
        vec![Part::from_text("For context:"), Part::from_text(line)],
        ROLE_USER,
    )
}

// parity: adapted: Rust renders empty maps as {} (real Go/Gemini wire is non-nil; nil vs empty unrepresentable); the agentTransfer foreign FC/FR lines assert {} where Go asserts null
#[tokio::test]
async fn test_contents_request_processor_include_contents() {
    const AGENT_NAME: &str = "testAgent";
    let hello_and_good_bye = vec![
        ev("user", text("hello", ROLE_USER)),
        ev("user", text("good bye", ROLE_USER)),
    ];
    let agent_transfer = vec![
        ev("anotherAgent", fc("func1", ROLE_MODEL)),
        ev("anotherAgent", fr("func1", ROLE_USER)),
        ev("anotherAgent", text("transfer to testAgent", ROLE_MODEL)),
        ev(AGENT_NAME, fc("func1", ROLE_MODEL)),
    ];
    let robot = vec![
        ev(AGENT_NAME, text("do func1", ROLE_USER)),
        ev(AGENT_NAME, fc("func1", ROLE_MODEL)),
        ev(AGENT_NAME, fr("func1", ROLE_USER)),
    ];
    let cases: Vec<(&str, IncludeContents, Vec<Event>, Vec<Content>)> = vec![
        ("empty", IncludeContents::Default, vec![], vec![]),
        ("empty", IncludeContents::None, vec![], vec![]),
        (
            "helloAndGoodBye",
            IncludeContents::Unset,
            hello_and_good_bye.clone(),
            vec![text("hello", ROLE_USER), text("good bye", ROLE_USER)],
        ),
        // Go: "same as \"\" here: nothing is bound, so there is no placement to opt out of".
        (
            "helloAndGoodBye",
            IncludeContents::Default,
            hello_and_good_bye.clone(),
            vec![text("hello", ROLE_USER), text("good bye", ROLE_USER)],
        ),
        (
            "helloAndGoodBye",
            IncludeContents::None,
            hello_and_good_bye.clone(),
            vec![text("good bye", ROLE_USER)],
        ),
        (
            "agentTransfer",
            IncludeContents::Unset,
            agent_transfer.clone(),
            vec![
                // Rendered as {} (see the adapted note on this fn).
                for_context("[anotherAgent] called tool `func1` with parameters: {}"),
                for_context("[anotherAgent] `func1` tool returned result: {}"),
                for_context("[anotherAgent] said: transfer to testAgent"),
                fc("func1", ROLE_MODEL),
            ],
        ),
        (
            "agentTransfer",
            IncludeContents::None,
            agent_transfer.clone(),
            vec![
                for_context("[anotherAgent] said: transfer to testAgent"),
                fc("func1", ROLE_MODEL),
            ],
        ),
        (
            "robot",
            IncludeContents::Default,
            robot.clone(),
            vec![
                text("do func1", ROLE_USER),
                fc("func1", ROLE_MODEL),
                fr("func1", ROLE_USER),
            ],
        ),
        (
            "robot",
            IncludeContents::None,
            robot.clone(),
            vec![
                text("do func1", ROLE_USER),
                fc("func1", ROLE_MODEL),
                fr("func1", ROLE_USER),
            ],
        ),
    ];
    for (name, include, events, want) in cases {
        let label = format!("{name}/include_contents={}", include.as_str());
        let agent = llm_agent(LlmAgentConfig {
            name: AGENT_NAME.into(),
            include_contents: include,
            ..Default::default()
        });
        let ctx = ctx_builder(&agent, events).build_invocation();
        let got = run_contents(&ctx)
            .await
            .unwrap_or_else(|e| panic!("{label}: contentRequestProcessor failed: {e}"));
        assert_eq!(with_continuation(want), got, "{label}");
    }
}

#[tokio::test]
async fn test_contents_request_processor_include_contents_none_isolation_scope_pivot() {
    let events = vec![
        ev("user", text("unscoped turn", ROLE_USER)),
        Event {
            isolation_scope: "task-1".into(),
            ..ev("user", text("scoped task", ROLE_USER))
        },
    ];
    let agent = llm_agent(LlmAgentConfig {
        name: "testAgent".into(),
        include_contents: IncludeContents::None,
        ..Default::default()
    });
    // Unscoped agent: the trailing scoped event must be skipped as the pivot, so the
    // unscoped user turn is what the agent sees.
    let ctx = ctx_builder(&agent, events).build_invocation();
    let got = run_contents(&ctx).await.unwrap();
    assert_eq!(
        with_continuation(vec![text("unscoped turn", ROLE_USER)]),
        got
    );
}

#[tokio::test]
async fn test_contents_request_processor_non_llm_agent() {
    let agent = new_agent(AgentConfig {
        name: "test_agent".into(),
        ..Default::default()
    })
    .unwrap();
    // Go passes no session; the builder's in-memory default session stands in.
    let ctx = InvocationContext::builder().agent(agent).build_invocation();
    let mut req = LlmRequest::default();
    let events = contents_request_processor(&ctx, &mut req, &Flow::default())
        .await
        .unwrap();
    assert!(
        events.is_empty(),
        "ContentsRequestProcessor generated an unexpected event"
    );
    // Go cmp.Diff(&model.LLMRequest{}, req): LlmRequest has no PartialEq, so every field is checked.
    assert!(req.model.is_empty(), "model: {:?}", req.model);
    assert!(req.contents.is_empty(), "contents: {:?}", req.contents);
    assert!(req.config.is_none(), "config was set");
    assert!(req.http_options.is_none(), "http_options was set");
    assert!(req.tools.is_empty(), "tools was set");
}

#[tokio::test]
async fn test_contents_request_processor_include_contents_none_ignores_sibling_branch_pivot() {
    const BRANCH: &str = "coordinator.translator_es";
    let events = vec![
        Event {
            branch: BRANCH.into(),
            ..ev("user", text("good morning", ROLE_USER))
        },
        Event {
            branch: "coordinator.translator_fr".into(),
            ..ev("translator_fr", text("Bonjour", ROLE_MODEL))
        },
    ];
    let agent = llm_agent(LlmAgentConfig {
        name: "translator_es".into(),
        mode: Mode::SingleTurn,
        include_contents: IncludeContents::None,
        ..Default::default()
    });
    let ctx = ctx_builder(&agent, events)
        .branch(BRANCH)
        .build_invocation();
    let got = run_contents(&ctx).await.unwrap();
    assert_eq!(vec![text("good morning", ROLE_USER)], got);
}

/// An event with nothing for the model to read, such as another agent's reply that carried
/// only grounding metadata or an error, cannot start the current turn: the user input before
/// it must stay in the request.
#[tokio::test]
#[ignore = "8a bug: IncludeContents::None starts the current turn at another agent's reply with nothing to read (grounding-only, error-only or partless; Go fc6de07d skips such a pivot)"]
async fn test_contents_request_processor_include_contents_none_skips_empty_pivot() {
    let cases = vec![
        (
            "grounding-only reply",
            LlmResponse {
                grounding_metadata: Some(gadk_genai::GroundingMetadata {
                    web_search_queries: vec!["X".into()],
                    ..Default::default()
                }),
                ..Default::default()
            },
        ),
        (
            "error reply",
            LlmResponse {
                error_code: "UNAVAILABLE".into(),
                error_message: "model unavailable".into(),
                ..Default::default()
            },
        ),
        (
            "reply with no parts",
            LlmResponse {
                content: Some(Content {
                    role: ROLE_MODEL.into(),
                    parts: vec![],
                }),
                ..Default::default()
            },
        ),
    ];
    for (name, reply) in cases {
        let events = vec![
            ev("user", text("Find X", ROLE_USER)),
            Event {
                author: "researcher".into(),
                response: reply,
                ..Default::default()
            },
        ];
        let summarizer = llm_agent(LlmAgentConfig {
            name: "summarizer".into(),
            include_contents: IncludeContents::None,
            ..Default::default()
        });
        let ctx = ctx_builder(&summarizer, events).build_invocation();
        let got = run_contents(&ctx)
            .await
            .unwrap_or_else(|e| panic!("{name}: ContentsRequestProcessor() error = {e}"));
        assert_eq!(vec![text("Find X", ROLE_USER)], got, "{name}");
    }
}

/// A tool result the caller posts back does not start the current turn. A long-running tool
/// is finished by running the agent again with its result as the new, user-authored message.
/// Anchoring there would cut the call the result answers out of the request. Mirrors
/// adk-python's `_is_submitted_tool_result` in `_get_current_turn_contents`.
#[tokio::test]
#[ignore = "8a bug: IncludeContents::None starts the current turn at a user-posted tool result, cutting out the call it answers (Go e847ba7d anchors at the user message that led to the call)"]
async fn test_contents_request_processor_include_contents_none_submitted_tool_result() {
    const AGENT_NAME: &str = "testAgent";
    let call_c =
        |id: &str| Content::from_parts(vec![fc_part(id, "long_job", json!({}))], ROLE_MODEL);
    let result_c = |id: &str, response: Value| {
        Content::from_parts(vec![fr_part(id, "long_job", response)], ROLE_USER)
    };
    let start = text("start the job", ROLE_USER);
    let call = call_c("call-1");
    let result = result_c("call-1", json!({"status": "done"}));
    let call_without_id = call_c("");
    let result_without_id = result_c("", json!({"status": "done"}));
    let orphan = result_c("no-such-call", json!({"status": "done"}));
    let question = text("is it done yet?", ROLE_USER);
    let not_yet = text("not yet", ROLE_MODEL);
    let earlier = text("hello", ROLE_USER);
    let earlier_reply = text("hi, what should I run?", ROLE_MODEL);
    // A tool that asks for confirmation answers with an error, the agent asks the user, and
    // the user's approval comes back as a function response.
    let confirmation_error = result_c("call-1", json!({"error": "confirmation required"}));
    let confirmation_request = Content::from_parts(
        vec![fc_part(
            "confirm-1",
            REQUEST_CONFIRMATION_FUNCTION_CALL_NAME,
            json!({}),
        )],
        ROLE_MODEL,
    );
    let approval = Content::from_parts(
        vec![fr_part(
            "confirm-1",
            REQUEST_CONFIRMATION_FUNCTION_CALL_NAME,
            json!({"confirmed": true}),
        )],
        ROLE_USER,
    );
    let e = |author: &str, c: &Content| {
        let mut out = ev(author, c.clone());
        if author == AGENT_NAME {
            out.long_running_tool_ids = c
                .parts
                .iter()
                .filter_map(|p| p.function_call.as_ref().map(|fc| fc.id.clone()))
                .collect();
        }
        out
    };
    let cases: Vec<(&str, Vec<Event>, Vec<Content>)> = vec![
        (
            "result right after the call",
            vec![e("user", &start), e(AGENT_NAME, &call), e("user", &result)],
            vec![start.clone(), call.clone(), result.clone()],
        ),
        (
            "user turn while the tool was pending",
            vec![
                e("user", &start),
                e(AGENT_NAME, &call),
                e("user", &question),
                e(AGENT_NAME, &not_yet),
                e("user", &result),
            ],
            vec![start.clone(), call.clone(), result.clone()],
        ),
        (
            // A finished turn ahead of the call: the anchor is the user message that led to
            // the call, not the first event.
            "earlier turn ahead of the call",
            vec![
                e("user", &earlier),
                e(AGENT_NAME, &earlier_reply),
                e("user", &start),
                e(AGENT_NAME, &call),
                e("user", &question),
                e(AGENT_NAME, &not_yet),
                e("user", &result),
            ],
            vec![start.clone(), call.clone(), result.clone()],
        ),
        (
            // Full-history mode pairs a call and a result that both lack an ID, so none mode
            // reaches back to such a call too.
            "call and result without IDs",
            vec![
                e("user", &start),
                e(AGENT_NAME, &call_without_id),
                e("user", &question),
                e(AGENT_NAME, &not_yet),
                e("user", &result_without_id),
            ],
            vec![
                start.clone(),
                call_without_id.clone(),
                result_without_id.clone(),
            ],
        ),
        (
            "tool resumed after the user confirms it",
            vec![
                e("user", &start),
                e(AGENT_NAME, &call),
                e(AGENT_NAME, &confirmation_error),
                e(AGENT_NAME, &confirmation_request),
                e("user", &approval),
                e(AGENT_NAME, &result),
            ],
            vec![start.clone(), call.clone(), result.clone()],
        ),
        (
            "result with no call stays out of the anchor search",
            vec![
                e("user", &start),
                e(AGENT_NAME, &call),
                e("user", &question),
                e("user", &orphan),
            ],
            vec![question.clone()],
        ),
    ];
    for (name, events, want) in cases {
        let agent = llm_agent(LlmAgentConfig {
            name: AGENT_NAME.into(),
            include_contents: IncludeContents::None,
            ..Default::default()
        });
        let ctx = ctx_builder(&agent, events).build_invocation();
        let got = run_contents(&ctx)
            .await
            .unwrap_or_else(|e| panic!("{name}: ContentsRequestProcessor() error = {e}"));
        assert_eq!(with_continuation(want), got, "{name}: contents");
    }
}

/// The parent's transfer_to_agent call and response do not start the current turn of the
/// agent it hands over to. Anchoring there would leave the user's request out of that agent's
/// prompt. Mirrors adk-python's `_is_direct_transfer` in `_get_current_turn_contents`. With
/// nothing earlier to start the turn, the latest hand-over still starts it.
#[tokio::test]
#[ignore = "8a bug: IncludeContents::None starts the current turn at the parent's transfer_to_agent hand-over, leaving the user's request out (Go 86620857 looks past hand-overs)"]
async fn test_contents_request_processor_include_contents_none_transferred_from_parent() {
    const PARENT_NAME: &str = "coordinator";
    const AGENT_NAME: &str = "geography";
    let question = text("What is the capital of France?", ROLE_USER);
    let transfer_call_part = fc_part(
        "call-1",
        "transfer_to_agent",
        json!({"agent_name": AGENT_NAME}),
    );
    let transfer_call = Content::from_parts(vec![transfer_call_part.clone()], ROLE_MODEL);
    let text_then_transfer_call = Content::from_parts(
        vec![Part::from_text("Handing over."), transfer_call_part],
        ROLE_MODEL,
    );
    let transfer_response = Content::from_parts(
        vec![fr_part("call-1", "transfer_to_agent", json!({}))],
        ROLE_USER,
    );
    let own_call = Content::from_parts(
        vec![fc_part("call-2", "lookup", json!({"country": "France"}))],
        ROLE_MODEL,
    );
    let own_response = Content::from_parts(
        vec![fr_part("call-2", "lookup", json!({"capital": "Paris"}))],
        ROLE_USER,
    );
    let user_event = ev("user", question.clone());
    let transfer_call_event = ev(PARENT_NAME, transfer_call);
    let transfer_response_event = {
        let mut out = ev(PARENT_NAME, transfer_response);
        out.actions.transfer_to_agent = AGENT_NAME.into();
        out
    };
    // An earlier turn ahead of the request. It stays out of the prompt, so a test passes only
    // if the turn starts at the request, not at the first event of the session.
    let earlier_answer_event = ev(AGENT_NAME, text("Madrid.", ROLE_MODEL));
    let earlier_turn = vec![
        ev("user", text("What is the capital of Spain?", ROLE_USER)),
        earlier_answer_event.clone(),
    ];
    let own_transfer_call = Content::from_parts(
        vec![fc_part(
            "call-4",
            "transfer_to_agent",
            json!({"agent_name": "history"}),
        )],
        ROLE_MODEL,
    );
    let own_transfer_error = Content::from_parts(
        vec![fr_part(
            "call-4",
            "transfer_to_agent",
            json!({"error": "agent history not found"}),
        )],
        ROLE_USER,
    );
    // A custom tool that hands over by setting Actions.TransferToAgent, with a call and
    // response named something other than transfer_to_agent.
    let handoff_call = Content::from_parts(
        vec![fc_part("call-3", "handoff", json!({"to": AGENT_NAME}))],
        ROLE_MODEL,
    );
    let handoff_response =
        Content::from_parts(vec![fr_part("call-3", "handoff", json!({}))], ROLE_USER);
    // The transfer itself stays in the prompt, as context from another agent.
    let transfer_call_context = for_context(
        r#"[coordinator] called tool `transfer_to_agent` with parameters: {"agent_name":"geography"}"#,
    );
    let transfer_response_context =
        for_context("[coordinator] `transfer_to_agent` tool returned result: {}");
    let with_earlier_turn = |events: Vec<Event>| {
        let mut out = earlier_turn.clone();
        out.extend(events);
        out
    };
    let router_event = {
        let mut out = ev(
            "router",
            Content::from_parts(
                vec![Part {
                    text: "Routing elsewhere.".into(),
                    thought: true,
                    ..Default::default()
                }],
                ROLE_MODEL,
            ),
        );
        out.actions.transfer_to_agent = "history".into();
        out
    };
    let handoff_response_event = {
        let mut out = ev(PARENT_NAME, handoff_response);
        out.actions.transfer_to_agent = AGENT_NAME.into();
        out
    };
    // (name, events, placed single turn, want)
    let cases: Vec<(&str, Vec<Event>, bool, Vec<Content>)> = vec![
        (
            "transfer is the latest event",
            with_earlier_turn(vec![
                user_event.clone(),
                transfer_call_event.clone(),
                transfer_response_event.clone(),
            ]),
            false,
            vec![
                question.clone(),
                transfer_call_context.clone(),
                transfer_response_context.clone(),
            ],
        ),
        (
            // The call is not the event's first part.
            "parent writes text before the transfer call",
            with_earlier_turn(vec![
                user_event.clone(),
                ev(PARENT_NAME, text_then_transfer_call),
                transfer_response_event.clone(),
            ]),
            false,
            vec![
                question.clone(),
                Content::from_parts(
                    vec![
                        Part::from_text("For context:"),
                        Part::from_text("[coordinator] said: Handing over."),
                        Part::from_text(
                            r#"[coordinator] called tool `transfer_to_agent` with parameters: {"agent_name":"geography"}"#,
                        ),
                    ],
                    ROLE_USER,
                ),
                transfer_response_context.clone(),
            ],
        ),
        (
            // The transfer is no longer the latest event on the agent's next model call,
            // after its own tool call.
            "agent's own tool call after the transfer",
            with_earlier_turn(vec![
                user_event.clone(),
                transfer_call_event.clone(),
                transfer_response_event.clone(),
                ev(AGENT_NAME, own_call.clone()),
                ev(AGENT_NAME, own_response.clone()),
            ]),
            false,
            vec![
                question.clone(),
                transfer_call_context.clone(),
                transfer_response_context.clone(),
                own_call.clone(),
                own_response.clone(),
            ],
        ),
        (
            // The session has no user event, so nothing ahead of the hand-over starts a turn.
            // The turn starts at the latest hand-over, as before hand-overs were skipped, so
            // the agent's own earlier answer stays out.
            "no user event in the session",
            vec![
                earlier_answer_event.clone(),
                transfer_call_event.clone(),
                transfer_response_event.clone(),
            ],
            false,
            vec![transfer_response_context.clone()],
        ),
        (
            // A later hand-over the agent cannot see, here one whose only part is a thought
            // that foreign conversion drops, is not the hand-over the turn falls back to. The
            // checks that hide an event from the agent run before the skip.
            "hand-over the agent cannot see after the hand-over",
            vec![
                earlier_answer_event.clone(),
                transfer_call_event.clone(),
                transfer_response_event.clone(),
                router_event,
            ],
            false,
            vec![transfer_response_context.clone()],
        ),
        (
            // The agent's own transfer_to_agent call never starts its turn, so the turn does
            // not fall back to it either. Here the agent named an unknown agent and is called
            // again with the error.
            "agent's own failed transfer after the hand-over",
            vec![
                earlier_answer_event.clone(),
                transfer_call_event.clone(),
                transfer_response_event.clone(),
                ev(AGENT_NAME, own_transfer_call.clone()),
                ev(AGENT_NAME, own_transfer_error.clone()),
            ],
            false,
            vec![
                transfer_response_context.clone(),
                own_transfer_call,
                own_transfer_error,
            ],
        ),
        (
            // The response is skipped through Actions.TransferToAgent alone. The call is not
            // named transfer_to_agent, so it starts the turn, as in adk-python.
            "custom tool hands over through Actions.TransferToAgent",
            with_earlier_turn(vec![
                user_event.clone(),
                ev(PARENT_NAME, handoff_call),
                handoff_response_event,
            ]),
            false,
            vec![
                for_context(
                    r#"[coordinator] called tool `handoff` with parameters: {"to":"geography"}"#,
                ),
                for_context("[coordinator] `handoff` tool returned result: {}"),
            ],
        ),
        (
            "single_turn placement with IncludeContents unset",
            with_earlier_turn(vec![
                user_event.clone(),
                transfer_call_event.clone(),
                transfer_response_event.clone(),
            ]),
            true,
            vec![
                question.clone(),
                transfer_call_context.clone(),
                transfer_response_context.clone(),
            ],
        ),
    ];
    for (name, events, placed_single_turn, want) in cases {
        let agent = llm_agent(LlmAgentConfig {
            name: AGENT_NAME.into(),
            include_contents: if placed_single_turn {
                IncludeContents::Unset
            } else {
                IncludeContents::None
            },
            ..Default::default()
        });
        let mut ctx = ctx_builder(&agent, events).build_invocation();
        if placed_single_turn {
            ctx = crate::llm::mode::with_bound_mode(
                &ctx,
                AGENT_NAME,
                agent.agent_state(),
                Mode::SingleTurn,
            );
        }
        let got = run_contents(&ctx)
            .await
            .unwrap_or_else(|e| panic!("{name}: ContentsRequestProcessor() error = {e}"));
        assert_eq!(want, got, "{name}: contents");
    }
}

#[tokio::test]
async fn test_contents_request_processor_strict_isolation_filter_excludes_foreign_scope() {
    const MY_SCOPE: &str = "task-mine";
    let events = vec![
        // Foreign-scoped user event: must be filtered out.
        Event {
            isolation_scope: "garbage-scope".into(),
            ..ev("user", text("foreign payload", ROLE_USER))
        },
        // In-scope user event: must reach the LLM.
        Event {
            isolation_scope: MY_SCOPE.into(),
            ..ev("user", text("my task brief", ROLE_USER))
        },
    ];
    let agent = llm_agent(LlmAgentConfig {
        name: "scopedAgent".into(),
        ..Default::default()
    });
    let ctx = ctx_builder(&agent, events)
        .isolation_scope(MY_SCOPE)
        .build_invocation();
    let got = run_contents(&ctx).await.unwrap();
    assert_eq!(
        with_continuation(vec![text("my task brief", ROLE_USER)]),
        got
    );
}

#[tokio::test]
async fn test_contents_request_processor_task_input_from_originating_fc() {
    const TASK_AGENT_NAME: &str = "taskAgent";
    const FC_ID: &str = "fc-1";
    let args = json!({"goal": "summarize"});
    let events = vec![
        ev(
            "coord",
            Content::from_parts(
                vec![Part {
                    function_call: Some(FunctionCall {
                        id: FC_ID.into(),
                        name: TASK_AGENT_NAME.into(),
                        args: args.as_object().cloned().unwrap(),
                        ..Default::default()
                    }),
                    ..Default::default()
                }],
                ROLE_MODEL,
            ),
        ),
        Event {
            isolation_scope: FC_ID.into(),
            ..ev(TASK_AGENT_NAME, text("working on it", ROLE_MODEL))
        },
    ];
    let args_json = go_json_marshal(&args);

    // task mode prepends FC args without nudge
    let task_agent = llm_agent(LlmAgentConfig {
        name: TASK_AGENT_NAME.into(),
        mode: Mode::Task,
        ..Default::default()
    });
    let ctx = ctx_builder(&task_agent, events.clone())
        .isolation_scope(FC_ID)
        .build_invocation();
    let got = run_contents(&ctx).await.unwrap();
    let want = vec![
        Content::from_parts(vec![Part::from_text(&args_json)], ROLE_USER),
        text("working on it", ROLE_MODEL),
    ];
    assert_eq!(
        with_continuation(want),
        got,
        "task mode prepends FC args without nudge"
    );

    // single_turn mode appends nudge after FC args
    let st_agent = llm_agent(LlmAgentConfig {
        name: TASK_AGENT_NAME.into(),
        mode: Mode::SingleTurn,
        include_contents: IncludeContents::None,
        ..Default::default()
    });
    let ctx = ctx_builder(&st_agent, events)
        .isolation_scope(FC_ID)
        .build_invocation();
    let got = run_contents(&ctx).await.unwrap();
    let want_leading = Content::from_parts(
        vec![
            Part::from_text(&args_json),
            Part::from_text(SINGLE_TURN_NUDGE),
        ],
        ROLE_USER,
    );
    assert!(!got.is_empty(), "expected at least one content; got none");
    assert_eq!(
        want_leading, got[0],
        "single_turn mode appends nudge after FC args"
    );
}

#[tokio::test]
async fn test_contents_request_processor_task_input_from_user_content_fallback() {
    const TASK_AGENT_NAME: &str = "taskAgent";
    const SCOPE: &str = "wf:node-1";
    let user_input = text("do the thing", ROLE_USER);
    // Session has no event carrying a FunctionCall with ID == scope.
    let events = vec![Event {
        isolation_scope: SCOPE.into(),
        ..ev(TASK_AGENT_NAME, text("ack", ROLE_MODEL))
    }];
    let task_agent = llm_agent(LlmAgentConfig {
        name: TASK_AGENT_NAME.into(),
        mode: Mode::Task,
        ..Default::default()
    });
    let ctx = ctx_builder(&task_agent, events)
        .isolation_scope(SCOPE)
        .user_content(user_input.clone())
        .build_invocation();
    let got = run_contents(&ctx).await.unwrap();
    let want = vec![
        Content::from_parts(user_input.parts.clone(), ROLE_USER),
        text("ack", ROLE_MODEL),
    ];
    assert_eq!(with_continuation(want), got);
}

fn obj(v: Value) -> Map<String, Value> {
    v.as_object().cloned().unwrap()
}

fn fc_part(id: &str, name: &str, args: Value) -> Part {
    Part {
        function_call: Some(FunctionCall {
            id: id.into(),
            name: name.into(),
            args: obj(args),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn fr_part(id: &str, name: &str, response: Value) -> Part {
    Part {
        function_response: Some(FunctionResponse {
            id: id.into(),
            name: name.into(),
            response: obj(response),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// A part referring to a file (Go `&genai.Part{FileData: &genai.FileData{FileURI, MIMEType}}`).
fn file_part(uri: &str, mime_type: &str) -> Part {
    Part {
        file_data: Some(gadk_genai::FileData {
            file_uri: uri.into(),
            mime_type: mime_type.into(),
            ..Default::default()
        }),
        ..Default::default()
    }
}

#[tokio::test]
#[ignore = "8a bug: the contents processor keeps internal artifact://…/_adk_live/… parts (and a foreign event made only of them becomes a bare \"For context:\" turn); Go 22ff1839 drops them"]
async fn test_contents_request_processor() {
    const AGENT_NAME: &str = "testAgent";
    let transcription = |author: &str, input: bool, t: &str, finished: bool| {
        let tr = Some(Transcription {
            text: t.into(),
            finished,
            ..Default::default()
        });
        let response = if input {
            LlmResponse {
                input_transcription: tr,
                ..Default::default()
            }
        } else {
            LlmResponse {
                output_transcription: tr,
                ..Default::default()
            }
        };
        Event {
            author: author.into(),
            response,
            ..Default::default()
        }
    };
    let on_branch = |branch: &str, t: &str| Event {
        branch: branch.into(),
        ..ev("user", text(t, ROLE_USER))
    };
    let in_scope = |scope: &str, t: &str| Event {
        isolation_scope: scope.into(),
        ..ev("user", text(t, ROLE_USER))
    };
    // (name, branch, isolation_scope, events, want)
    type Case<'a> = (&'a str, &'a str, &'a str, Vec<Event>, Vec<Content>);
    let cases: Vec<Case> = vec![
        ("NilEvent", "", "", vec![], vec![]),
        ("EmptyEvents", "", "", vec![], vec![]),
        (
            "UserAndAgentEvents",
            "",
            "",
            vec![
                ev("user", text("Hello", ROLE_USER)),
                ev("testAgent", text("Hi there", ROLE_MODEL)),
            ],
            vec![text("Hello", ROLE_USER), text("Hi there", ROLE_MODEL)],
        ),
        (
            "anotherAgentEvent",
            "",
            "",
            vec![ev("anotherAgent", text("Foreign message", ROLE_MODEL))],
            vec![for_context("[anotherAgent] said: Foreign message")],
        ),
        (
            "FilterAdkLiveArtifacts",
            "",
            "",
            vec![ev(
                "user",
                Content::from_parts(
                    vec![
                        Part::from_text("Some text"),
                        file_part("artifact://app/user/session/_adk_live/audio.pcm#1", ""),
                    ],
                    ROLE_USER,
                ),
            )],
            vec![Content::from_parts(
                vec![Part::from_text("Some text")],
                ROLE_USER,
            )],
        ),
        (
            // The audio cache manager writes an event whose only part is the internal
            // reference, so the whole content has to go.
            "FilterAdkLiveArtifactOnlyContent",
            "",
            "",
            vec![
                ev("user", text("before", ROLE_USER)),
                ev(
                    "user",
                    Content::from_parts(
                        vec![file_part(
                            "artifact://app/user/session/_adk_live/adk_live_audio_storage_input_audio_1700000000000.pcm#0",
                            "audio/pcm",
                        )],
                        ROLE_USER,
                    ),
                ),
                ev("user", text("after", ROLE_USER)),
            ],
            vec![text("before", ROLE_USER), text("after", ROLE_USER)],
        ),
        (
            // With SaveLiveBlob on, the output-audio event is authored by the agent that
            // spoke, so another agent sees it as a foreign event. ConvertForeignEvent must
            // not turn it into a bare "For context:" turn: an event whose only parts are
            // internal live refs has to behave as if it were not in the session at all.
            "DropForeignAdkLiveArtifactOnlyContent",
            "",
            "",
            vec![
                ev("user", text("hi", ROLE_USER)),
                ev(
                    "otherAgent",
                    Content::from_parts(
                        vec![file_part(
                            "artifact://app/user/session/_adk_live/adk_live_audio_storage_output_audio_1700000000000.pcm#0",
                            "audio/pcm",
                        )],
                        ROLE_MODEL,
                    ),
                ),
            ],
            vec![text("hi", ROLE_USER)],
        ),
        (
            // A foreign event that also says something keeps the spoken part and loses only
            // the internal ref.
            "FilterForeignAdkLiveArtifactKeepsText",
            "",
            "",
            vec![ev(
                "otherAgent",
                Content::from_parts(
                    vec![
                        Part::from_text("here is the summary"),
                        file_part(
                            "artifact://app/user/session/_adk_live/adk_live_audio_storage_output_audio_1700000000000.pcm#0",
                            "audio/pcm",
                        ),
                    ],
                    ROLE_MODEL,
                ),
            )],
            vec![for_context("[otherAgent] said: here is the summary")],
        ),
        (
            // A user-visible artifact outside the _adk_live namespace stays.
            "KeepNonLiveArtifactRefs",
            "",
            "",
            vec![ev(
                "user",
                Content::from_parts(
                    vec![
                        Part::from_text("look at this"),
                        file_part(
                            "artifact://app/user/session/report.pdf#1",
                            "application/pdf",
                        ),
                        file_part("gs://bucket/recording.mp3", "audio/mp3"),
                    ],
                    ROLE_USER,
                ),
            )],
            vec![Content::from_parts(
                vec![
                    Part::from_text("look at this"),
                    file_part(
                        "artifact://app/user/session/report.pdf#1",
                        "application/pdf",
                    ),
                    file_part("gs://bucket/recording.mp3", "audio/mp3"),
                ],
                ROLE_USER,
            )],
        ),
        (
            "ExcludeToolConfirmation",
            "",
            "",
            vec![
                ev(
                    "AgentA",
                    Content::from_parts(
                        vec![fc_part(
                            "call_confirm_123",
                            REQUEST_CONFIRMATION_FUNCTION_CALL_NAME,
                            json!({"message": "Confirm delete?"}),
                        )],
                        ROLE_MODEL,
                    ),
                ),
                ev(
                    "user",
                    Content::from_parts(
                        vec![fr_part(
                            "call_confirm_123",
                            REQUEST_CONFIRMATION_FUNCTION_CALL_NAME,
                            json!({"confirmed": true}),
                        )],
                        ROLE_USER,
                    ),
                ),
            ],
            vec![],
        ),
        (
            "FilterByBranch",
            "branch1.task1",
            "",
            vec![
                on_branch("branch1", "In branch 1"),
                on_branch("branch1.task1", "In branch 1 and task 1"),
                on_branch("branch12", "In branch 12"),
                on_branch("branch2", "In branch 2"),
                on_branch("", "empty branch"),
            ],
            vec![
                text("In branch 1", ROLE_USER),
                text("In branch 1 and task 1", ROLE_USER),
                text("empty branch", ROLE_USER),
            ],
        ),
        // Isolation scope is exact-match (unlike branch, where empty is universally visible):
        // a scoped agent sees ONLY events with the same scope, not unscoped ones.
        (
            "FilterByIsolationScope_Scoped",
            "",
            "task-1",
            vec![
                in_scope("task-1", "in task 1"),
                in_scope("task-2", "in task 2"),
                in_scope("", "unscoped"),
            ],
            vec![text("in task 1", ROLE_USER)],
        ),
        // An unscoped agent (empty scope) sees ONLY unscoped events.
        (
            "FilterByIsolationScope_Unscoped",
            "",
            "",
            vec![in_scope("task-1", "in task 1"), in_scope("", "unscoped")],
            vec![text("unscoped", ROLE_USER)],
        ),
        (
            "AuthEvent",
            "",
            "",
            vec![ev(
                AGENT_NAME,
                Content::from_parts(
                    vec![Part {
                        function_call: Some(FunctionCall {
                            name: "adk_request_credential".into(),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }],
                    ROLE_MODEL,
                ),
            )],
            vec![],
        ),
        (
            "EventWithoutContent",
            "",
            "",
            vec![Event {
                author: "user".into(),
                ..Default::default()
            }],
            vec![],
        ),
        (
            "TranscriptionAggregation",
            "",
            "",
            vec![
                transcription("user", true, "hello ", false),
                transcription("user", true, "world", true),
                transcription("testAgent", false, "hi ", false),
                transcription("testAgent", false, "there", true),
                transcription("user", true, "ok", true),
            ],
            vec![
                text("hello world", ROLE_USER),
                text("hi there", ROLE_MODEL),
                text("ok", ROLE_USER),
            ],
        ),
    ];
    for (name, branch, scope, events, want) in cases {
        let agent = llm_agent(LlmAgentConfig {
            name: "testAgent".into(),
            ..Default::default()
        });
        let ctx = ctx_builder(&agent, events)
            .branch(branch)
            .isolation_scope(scope)
            .build_invocation();
        let got = run_contents(&ctx)
            .await
            .unwrap_or_else(|e| panic!("{name}: contentRequestProcessor failed: {e}"));
        assert_eq!(with_continuation(want), got, "{name}");
    }
}

#[test]
fn test_convert_foreign_event() {
    let now = Utc::now();
    let foreign = |parts: Vec<Part>| Event {
        timestamp: now,
        author: "foreign".into(),
        branch: "b".into(),
        response: LlmResponse {
            content: Some(Content::from_parts(parts, ROLE_MODEL)),
            ..Default::default()
        },
        ..Default::default()
    };
    let converted = |line: &str| {
        Some(Event {
            timestamp: now,
            author: "user".into(),
            branch: "b".into(),
            response: LlmResponse {
                content: Some(for_context(line)),
                ..Default::default()
            },
            ..Default::default()
        })
    };
    let thought = |t: &str| Part {
        text: t.into(),
        thought: true,
        ..Default::default()
    };
    let cases: Vec<(&str, Event, Option<Event>)> = vec![
        (
            "Text",
            foreign(vec![Part::from_text("hello")]),
            converted("[foreign] said: hello"),
        ),
        (
            "FunctionCall",
            foreign(vec![fc_part("", "test", json!({"a": "b"}))]),
            converted("[foreign] called tool `test` with parameters: {\"a\":\"b\"}"),
        ),
        (
            "FunctionResponse",
            foreign(vec![fr_part("", "test", json!({"c": "d"}))]),
            converted("[foreign] `test` tool returned result: {\"c\":\"d\"}"),
        ),
        // A thought part (with text) must be excluded so another agent's private reasoning
        // never leaks into the current agent's context; the non-thought text is still presented.
        (
            "ThoughtWithTextExcluded",
            foreign(vec![
                thought("secret reasoning"),
                Part::from_text("the answer"),
            ]),
            converted("[foreign] said: the answer"),
        ),
        // An empty-text thought part must be dropped too.
        (
            "EmptyTextThoughtExcluded",
            foreign(vec![thought(""), Part::from_text("the answer")]),
            converted("[foreign] said: the answer"),
        ),
        // A foreign turn made up entirely of thoughts is dropped rather than emitting a bare header.
        (
            "ThoughtOnlyDropped",
            foreign(vec![thought("only reasoning")]),
            None,
        ),
        (
            "MultipleThoughtsOnlyDropped",
            foreign(vec![
                thought("internal reasoning"),
                thought("more thoughts"),
            ]),
            None,
        ),
    ];
    for (name, event, want) in cases {
        let got = convert_foreign_event(&event);
        assert_eq!(
            want, got,
            "{name}: converted event does not match the expected public content and metadata"
        );
    }
}

fn call(id: &str, name: &str, args: Value) -> FunctionCall {
    FunctionCall {
        id: id.into(),
        name: name.into(),
        args: obj(args),
        ..Default::default()
    }
}

fn resp(id: &str, name: &str, response: Value) -> FunctionResponse {
    FunctionResponse {
        id: id.into(),
        name: name.into(),
        response: obj(response),
        ..Default::default()
    }
}

/// Go `NewContentFromFunctionCall(fc, role)` (the test-file helper).
fn cfc(fc: &FunctionCall, role: &str) -> Content {
    Content::from_function_call(fc.clone(), role)
}

/// Go `NewContentFromFunctionResponse(fr, role)` (the test-file helper).
fn cfr(fr: &FunctionResponse, role: &str) -> Content {
    Content::from_function_response(fr.clone(), role)
}

fn pfc(fc: &FunctionCall) -> Part {
    Part {
        function_call: Some(fc.clone()),
        ..Default::default()
    }
}

fn pfr(fr: &FunctionResponse) -> Part {
    Part {
        function_response: Some(fr.clone()),
        ..Default::default()
    }
}

fn ptext(t: &str) -> Part {
    Part::from_text(t)
}

#[tokio::test]
async fn test_contents_request_processor_rearrange() {
    const A: &str = "test_agent";
    const U: &str = "user";
    const M: &str = ROLE_MODEL;

    // --- Reusable Test Data ---
    // Basic Call/Response
    let fc_basic = call("call_123", "search_tool", json!({"query": "test"}));
    let fr_basic = resp(
        "call_123",
        "search_tool",
        json!({"results": ["item1", "item2"]}),
    );

    // LRO Call/Responses
    let fc_lro = call(
        "long_call_123",
        "long_running_tool",
        json!({"task": "process"}),
    );
    let fr_lro_inter = resp(
        "long_call_123",
        "long_running_tool",
        json!({"status": "processing", "progress": 50}),
    );
    let fr_lro_final = resp(
        "long_call_123",
        "long_running_tool",
        json!({"status": "completed", "result": "done"}),
    );

    // Mixed LRO/Normal Calls/Responses
    let fc_lro_mixed = call(
        "lro_call_456",
        "long_running_tool",
        json!({"task": "analyze"}),
    );
    let fc_normal_mixed = call("normal_call_789", "search_tool", json!({"query": "test"}));
    let fr_lro_inter_mixed = resp(
        "lro_call_456",
        "long_running_tool",
        json!({"status": "processing", "progress": 25}),
    );
    let fr_normal_mixed = resp(
        "normal_call_789",
        "search_tool",
        json!({"results": ["item1", "item2"]}),
    );
    let fr_lro_final_mixed = resp(
        "lro_call_456",
        "long_running_tool",
        json!({"status": "completed", "analysis": "done"}),
    );

    // History LRO Call/Responses
    let fc_hist_lro = call(
        "history_call_123",
        "long_running_tool",
        json!({"task": "process"}),
    );
    let fr_hist_lro_inter = resp(
        "history_call_123",
        "long_running_tool",
        json!({"status": "processing", "progress": 50}),
    );
    let fr_hist_lro_final = resp(
        "history_call_123",
        "long_running_tool",
        json!({"status": "completed", "result": "done"}),
    );

    // History Mixed Call/Responses
    let fc_hist_lro_mixed = call(
        "history_lro_123",
        "long_running_tool",
        json!({"task": "analyze"}),
    );
    let fc_hist_normal_mixed = call(
        "history_normal_456",
        "search_tool",
        json!({"query": "data"}),
    );
    let fr_hist_lro_inter_mixed = resp(
        "history_lro_123",
        "long_running_tool",
        json!({"status": "processing", "progress": 30}),
    );
    let fr_hist_normal_mixed = resp(
        "history_normal_456",
        "search_tool",
        json!({"results": ["result1", "result2"]}),
    );
    let fr_hist_lro_final_mixed = resp(
        "history_lro_123",
        "long_running_tool",
        json!({"status": "completed", "analysis": "finished"}),
    );

    // Preserve Content Call/Responses
    let fc_preserve = call(
        "preserve_test",
        "long_running_tool",
        json!({"test": "value"}),
    );
    let fr_preserve_inter = resp(
        "preserve_test",
        "long_running_tool",
        json!({"status": "processing"}),
    );
    let fr_preserve_final = resp(
        "preserve_test",
        "long_running_tool",
        json!({"output": "preserved"}),
    );

    // Human-in-the-loop confirmation call/responses
    let hint = "Please approve or reject the tool call request_vacation_days().";
    let confirmation_args = |original: &FunctionCall| {
        json!({
            "originalFunctionCall": original,
            "toolConfirmation": ToolConfirmation { hint: hint.into(), confirmed: false, payload: None },
        })
    };
    let fc_hitl_approved = call(
        "hitl_approved_call",
        "request_vacation_days",
        json!({"days": 5, "user_id": "user-123"}),
    );
    let fr_hitl_approved_pending = resp(
        "hitl_approved_call",
        "request_vacation_days",
        json!({"status": "Manager approval is required.", "request_id": "req-1"}),
    );
    let fr_hitl_approved_final = resp(
        "hitl_approved_call",
        "request_vacation_days",
        json!({"status": "The time off request is accepted.", "days_approved": 5, "request_id": "req-1"}),
    );
    let fc_hitl_approval_request = call(
        "hitl_approved_confirmation",
        REQUEST_CONFIRMATION_FUNCTION_CALL_NAME,
        confirmation_args(&fc_hitl_approved),
    );
    let fr_hitl_approval_confirmed = resp(
        "hitl_approved_confirmation",
        REQUEST_CONFIRMATION_FUNCTION_CALL_NAME,
        json!({"confirmed": true}),
    );
    let fc_hitl_denied = call(
        "hitl_denied_call",
        "request_vacation_days",
        json!({"days": 10, "user_id": "user-123"}),
    );
    let fr_hitl_denied_pending = resp(
        "hitl_denied_call",
        "request_vacation_days",
        json!({"status": "Manager approval is required.", "request_id": "req-2"}),
    );
    let fr_hitl_denied_final = resp(
        "hitl_denied_call",
        "request_vacation_days",
        json!({"status": "The time off request is rejected.", "days_approved": 0, "request_id": "req-2"}),
    );
    let fc_hitl_denial_request = call(
        "hitl_denied_confirmation",
        REQUEST_CONFIRMATION_FUNCTION_CALL_NAME,
        confirmation_args(&fc_hitl_denied),
    );
    let fr_hitl_denial_rejected = resp(
        "hitl_denied_confirmation",
        REQUEST_CONFIRMATION_FUNCTION_CALL_NAME,
        json!({"confirmed": false}),
    );

    // Late async completion: a long-running call whose final response arrives as the latest
    // event, after an unrelated tool exchange.
    let fc_async_plan = call("async_plan_1", "plan_tool", json!({"feature": "Q"}));
    let fr_async_plan_ack = resp("async_plan_1", "plan_tool", json!({"status": "accepted"}));
    let fr_async_plan_final = resp(
        "async_plan_1",
        "plan_tool",
        json!({"status": "completed", "plan": "add the flag, then wire it up"}),
    );
    let fc_async_status = call("async_status_1", "status_tool", json!({"job": "plan"}));
    let fr_async_status = resp(
        "async_status_1",
        "status_tool",
        json!({"status": "running", "elapsed": "21s"}),
    );
    let fc_async_unanswered = call(
        "async_unanswered_1",
        "notify_tool",
        json!({"channel": "ops"}),
    );

    // Error Call/Response
    let fr_orphaned = resp(
        "no_matching_call",
        "orphaned_tool",
        json!({"error": "no matching call"}),
    );
    let fr_without_id = resp("", "legacy_tool", json!({"result": "ok"}));
    let fc_other = call("call_456", "other_tool", json!({"query": "other"}));
    let fr_other = resp("call_456", "other_tool", json!({"result": "other"}));

    let parts = |parts: Vec<Part>, role: &str| Content::from_parts(parts, role);
    let long_running = |e: Event, id: &str| Event {
        long_running_tool_ids: vec![id.to_owned()],
        ..e
    };
    let image = || Blob {
        mime_type: "image/png".into(),
        data: b"image".to_vec().into(),
        ..Default::default()
    };

    struct Case {
        name: &'static str,
        events: Vec<Event>,
        want: Vec<Content>,
        want_err: Option<&'static str>,
        include_contents: IncludeContents,
    }
    let case = |name: &'static str, events: Vec<Event>, want: Vec<Content>| Case {
        name,
        events,
        want,
        want_err: None,
        include_contents: IncludeContents::Unset,
    };

    // --- Test Cases ---
    let cases = vec![
        case("NilEvent", vec![], vec![]),
        case("EmptyEvents", vec![], vec![]),
        case(
            "EventWithoutContent",
            vec![Event {
                author: "user".into(),
                ..Default::default()
            }],
            vec![],
        ),
        case(
            "Basic function call no rearrangement",
            vec![
                ev(U, text("Search for test", U)),
                ev(A, cfc(&fc_basic, M)),
                ev(U, cfr(&fr_basic, U)),
            ],
            vec![
                text("Search for test", U),
                cfc(&fc_basic, M),
                cfr(&fr_basic, U),
            ],
        ),
        case(
            "Rearrangement with intermediate response",
            vec![
                ev(U, text("Run long process", U)),
                ev(A, cfc(&fc_lro, M)),
                ev(U, cfr(&fr_lro_inter, U)),
                ev(A, text("Still processing...", M)),
                ev(U, cfr(&fr_lro_final, U)),
            ],
            vec![
                text("Run long process", U),
                cfc(&fc_lro, M),
                cfr(&fr_lro_final, U),
            ],
        ),
        // The unrelated search exchange survives (the property #767 established) but sits
        // before the completion, so the freshest response is the final content.
        case(
            "Rearrangement preserves unrelated function events",
            vec![
                ev(U, text("Run long process and search", U)),
                ev(A, cfc(&fc_lro, M)),
                ev(U, cfr(&fr_lro_inter, U)),
                ev(A, cfc(&fc_basic, M)),
                ev(U, cfr(&fr_basic, U)),
                ev(U, cfr(&fr_lro_final, U)),
            ],
            vec![
                text("Run long process and search", U),
                cfc(&fc_basic, M),
                cfr(&fr_basic, U),
                cfc(&fc_lro, M),
                cfr(&fr_lro_final, U),
            ],
        ),
        // Regression test for #1181: the completion of a long-running tool must stay the final
        // content, not be dragged back next to its much earlier call while a stale status
        // exchange takes the tail.
        case(
            "Late async completion stays the final content",
            vec![
                ev(U, text("Plan how to add feature Q", U)),
                ev(A, cfc(&fc_async_plan, M)),
                ev(U, cfr(&fr_async_plan_ack, U)),
                ev(A, text("I dispatched the planning task.", M)),
                ev(U, text("What is the status?", U)),
                ev(A, cfc(&fc_async_status, M)),
                ev(U, cfr(&fr_async_status, U)),
                ev(A, text("Still running.", M)),
                ev(U, cfr(&fr_async_plan_final, U)),
            ],
            vec![
                text("Plan how to add feature Q", U),
                cfc(&fc_async_status, M),
                cfr(&fr_async_status, U),
                cfc(&fc_async_plan, M),
                cfr(&fr_async_plan_final, U),
            ],
        ),
        // A call that never receives a response (a pending long-running call listed in
        // LongRunningToolIDs) keeps its order relative to the events that stay put, which
        // leaves it ahead of the completed pair that moves to the tail.
        case(
            "Late async completion stays final with an unanswered call in history",
            vec![
                ev(U, text("Plan how to add feature Q", U)),
                ev(A, cfc(&fc_async_plan, M)),
                ev(U, cfr(&fr_async_plan_ack, U)),
                long_running(ev(A, cfc(&fc_async_unanswered, M)), &fc_async_unanswered.id),
                ev(U, cfr(&fr_async_plan_final, U)),
            ],
            vec![
                text("Plan how to add feature Q", U),
                cfc(&fc_async_unanswered, M),
                cfc(&fc_async_plan, M),
                cfr(&fr_async_plan_final, U),
            ],
        ),
        // The same history when the unanswered call is not pending: a turn interrupted before
        // the tool ran. The call is dropped.
        case(
            "Late async completion drops an interrupted call from history",
            vec![
                ev(U, text("Plan how to add feature Q", U)),
                ev(A, cfc(&fc_async_plan, M)),
                ev(U, cfr(&fr_async_plan_ack, U)),
                ev(A, cfc(&fc_async_unanswered, M)),
                ev(U, cfr(&fr_async_plan_final, U)),
            ],
            vec![
                text("Plan how to add feature Q", U),
                cfc(&fc_async_plan, M),
                cfr(&fr_async_plan_final, U),
            ],
        ),
        // A turn that ended between a call and its result, followed by a new user message.
        case(
            "Unanswered call at the end of a turn is dropped",
            vec![
                ev(U, text("search for test", U)),
                ev(A, cfc(&fc_async_unanswered, M)),
                ev(U, text("are you still there?", U)),
            ],
            vec![text("search for test", U), text("are you still there?", U)],
        ),
        case(
            "Unanswered call mid-history is dropped and the text around it kept",
            vec![
                ev(U, text("Plan how to add feature Q", U)),
                ev(A, cfc(&fc_async_status, M)),
                ev(U, cfr(&fr_async_status, U)),
                ev(
                    A,
                    parts(
                        vec![ptext("Notifying ops first."), pfc(&fc_async_unanswered)],
                        M,
                    ),
                ),
                ev(U, text("Cancel that", U)),
            ],
            vec![
                text("Plan how to add feature Q", U),
                cfc(&fc_async_status, M),
                cfr(&fr_async_status, U),
                text("Notifying ops first.", M),
                text("Cancel that", U),
            ],
        ),
        // Dropping the trailing call must not leave the async response as the last event, which
        // would let the latest-response rearrangement discard the text turns between it and its
        // call.
        case(
            "Trailing unanswered call after an async completion keeps the interleaved turns",
            vec![
                ev(U, text("Plan how to add feature Q", U)),
                ev(A, cfc(&fc_async_plan, M)),
                ev(U, text("While waiting, tell me a joke", U)),
                ev(A, text("Why did the chicken cross the road?", M)),
                ev(U, cfr(&fr_async_plan_final, U)),
                ev(A, cfc(&fc_async_unanswered, M)),
            ],
            vec![
                text("Plan how to add feature Q", U),
                cfc(&fc_async_plan, M),
                cfr(&fr_async_plan_final, U),
                text("While waiting, tell me a joke", U),
                text("Why did the chicken cross the road?", M),
            ],
        ),
        case(
            "Pending long-running call stays in history",
            vec![
                ev(U, text("Plan how to add feature Q", U)),
                long_running(ev(A, cfc(&fc_async_unanswered, M)), &fc_async_unanswered.id),
                ev(U, text("Any update?", U)),
            ],
            vec![
                text("Plan how to add feature Q", U),
                cfc(&fc_async_unanswered, M),
                text("Any update?", U),
            ],
        ),
        case(
            "HITL confirmation approved preserves resumed tool response",
            vec![
                ev(U, text("Request five vacation days", U)),
                ev(A, cfc(&fc_hitl_approved, M)),
                ev(A, cfr(&fr_hitl_approved_pending, U)),
                ev(A, cfc(&fc_hitl_approval_request, M)),
                ev(U, cfr(&fr_hitl_approval_confirmed, U)),
                ev(A, cfr(&fr_hitl_approved_final, U)),
            ],
            vec![
                text("Request five vacation days", U),
                cfc(&fc_hitl_approved, M),
                cfr(&fr_hitl_approved_final, U),
            ],
        ),
        case(
            "HITL confirmation denied preserves rejected tool response",
            vec![
                ev(U, text("Request ten vacation days", U)),
                ev(A, cfc(&fc_hitl_denied, M)),
                ev(A, cfr(&fr_hitl_denied_pending, U)),
                ev(A, cfc(&fc_hitl_denial_request, M)),
                ev(U, cfr(&fr_hitl_denial_rejected, U)),
                ev(A, cfr(&fr_hitl_denied_final, U)),
            ],
            vec![
                text("Request ten vacation days", U),
                cfc(&fc_hitl_denied, M),
                cfr(&fr_hitl_denied_final, U),
            ],
        ),
        case(
            "Rearrangement with mixed LRO and normal calls",
            vec![
                ev(U, text("Analyze data and search for info", U)),
                ev(A, parts(vec![pfc(&fc_lro_mixed), pfc(&fc_normal_mixed)], M)),
                ev(
                    U,
                    parts(vec![pfr(&fr_lro_inter_mixed), pfr(&fr_normal_mixed)], U),
                ),
                ev(A, text("Analysis in progress, search completed", M)),
                ev(U, cfr(&fr_lro_final_mixed, U)),
            ],
            vec![
                text("Analyze data and search for info", U),
                parts(vec![pfc(&fc_lro_mixed), pfc(&fc_normal_mixed)], M),
                parts(vec![pfr(&fr_lro_final_mixed), pfr(&fr_normal_mixed)], U),
            ],
        ),
        case(
            "Rearrangement in history (non-final event)",
            vec![
                ev(U, text("Start long process", U)),
                ev(A, cfc(&fc_hist_lro, M)),
                ev(U, cfr(&fr_hist_lro_inter, U)),
                ev(A, text("Still processing...", M)),
                ev(U, cfr(&fr_hist_lro_final, U)),
                ev(A, text("Process completed successfully!", M)),
                ev(U, text("Great! What's next?", U)),
            ],
            vec![
                text("Start long process", U),
                cfc(&fc_hist_lro, M),
                cfr(&fr_hist_lro_final, U),
                text("Still processing...", M),
                text("Process completed successfully!", M),
                text("Great! What's next?", U),
            ],
        ),
        case(
            "Mixed rearrangement in history (non-final event)",
            vec![
                ev(U, text("Analyze and search simultaneously", U)),
                ev(
                    A,
                    parts(vec![pfc(&fc_hist_lro_mixed), pfc(&fc_hist_normal_mixed)], M),
                ),
                ev(
                    U,
                    parts(
                        vec![pfr(&fr_hist_lro_inter_mixed), pfr(&fr_hist_normal_mixed)],
                        U,
                    ),
                ),
                ev(A, text("Analysis continuing, search done", M)),
                ev(U, cfr(&fr_hist_lro_final_mixed, U)),
                ev(A, text("Both tasks completed successfully!", M)),
                ev(U, text("Perfect! What should we do next?", U)),
            ],
            vec![
                text("Analyze and search simultaneously", U),
                parts(vec![pfc(&fc_hist_lro_mixed), pfc(&fc_hist_normal_mixed)], M),
                parts(
                    vec![pfr(&fr_hist_lro_final_mixed), pfr(&fr_hist_normal_mixed)],
                    U,
                ),
                text("Analysis continuing, search done", M),
                text("Both tasks completed successfully!", M),
                text("Perfect! What should we do next?", U),
            ],
        ),
        case(
            "Rearrangement preserves mixed text parts",
            vec![
                ev(U, text("Before function call", U)),
                ev(
                    A,
                    parts(
                        vec![ptext("I'll process this for you"), pfc(&fc_preserve)],
                        M,
                    ),
                ),
                ev(
                    U,
                    parts(
                        vec![
                            ptext("Intermediate prefix"),
                            pfr(&fr_preserve_inter),
                            ptext("Processing..."),
                        ],
                        U,
                    ),
                ),
                ev(A, text("Still working on it...", M)),
                ev(
                    U,
                    parts(
                        vec![
                            ptext("Final prefix"),
                            pfr(&fr_preserve_final),
                            ptext("Final suffix"),
                        ],
                        U,
                    ),
                ),
            ],
            vec![
                text("Before function call", U),
                parts(
                    vec![ptext("I'll process this for you"), pfc(&fc_preserve)],
                    M,
                ),
                parts(
                    vec![
                        ptext("Intermediate prefix"),
                        pfr(&fr_preserve_final),
                        ptext("Processing..."),
                        ptext("Final prefix"),
                        ptext("Final suffix"),
                    ],
                    U,
                ),
            ],
        ),
        case(
            "Orphaned function response without matching call is dropped",
            vec![
                ev(U, parts(vec![ptext("Regular message")], U)),
                ev(U, parts(vec![pfr(&fr_orphaned)], U)),
            ],
            vec![text("Regular message", U)],
        ),
        case(
            "Orphaned function response after text reply is dropped",
            vec![
                ev(U, text("Hello", U)),
                ev(A, text("Hi there", M)),
                ev(U, parts(vec![pfr(&fr_orphaned)], U)),
            ],
            vec![text("Hello", U), text("Hi there", M)],
        ),
        case(
            "Orphaned function response is removed from mixed content",
            vec![
                ev(U, text("Hello", U)),
                ev(A, text("Hi there", M)),
                ev(
                    U,
                    parts(vec![ptext("What is the weather?"), pfr(&fr_orphaned)], U),
                ),
            ],
            vec![
                text("Hello", U),
                text("Hi there", M),
                text("What is the weather?", U),
            ],
        ),
        case(
            "Orphaned function response sharing a part preserves text",
            vec![
                ev(U, text("Hello", U)),
                ev(A, text("Hi", M)),
                ev(
                    U,
                    parts(
                        vec![Part {
                            function_response: Some(fr_orphaned.clone()),
                            ..ptext("What is the weather?")
                        }],
                        U,
                    ),
                ),
            ],
            vec![
                text("Hello", U),
                text("Hi", M),
                text("What is the weather?", U),
            ],
        ),
        case(
            "Rearrangement preserves text sharing an orphaned response part",
            vec![
                ev(A, cfc(&fc_basic, M)),
                ev(
                    U,
                    parts(
                        vec![Part {
                            function_response: Some(fr_orphaned.clone()),
                            ..ptext("note")
                        }],
                        U,
                    ),
                ),
                ev(U, cfr(&fr_basic, U)),
            ],
            vec![text("note", U), cfc(&fc_basic, M), cfr(&fr_basic, U)],
        ),
        case(
            "Rearrangement preserves separate text beside an orphaned response",
            vec![
                ev(A, cfc(&fc_basic, M)),
                // Ordinary text without an orphaned response still follows the existing
                // intermediate-event filtering rule.
                ev(U, text("plain note", U)),
                ev(U, parts(vec![ptext("note"), pfr(&fr_orphaned)], U)),
                ev(U, cfr(&fr_basic, U)),
            ],
            vec![text("note", U), cfc(&fc_basic, M), cfr(&fr_basic, U)],
        ),
        // The latest call/response pair stays last, so retaining model text does not trigger a
        // synthetic user continuation.
        case(
            "Rearrangement preserves model orphan remnant without continuation",
            vec![
                ev(A, cfc(&fc_basic, M)),
                ev(
                    A,
                    parts(
                        vec![Part {
                            function_response: Some(fr_orphaned.clone()),
                            ..ptext("note")
                        }],
                        M,
                    ),
                ),
                ev(U, cfr(&fr_basic, U)),
            ],
            vec![text("note", M), cfc(&fc_basic, M), cfr(&fr_basic, U)],
        ),
        case(
            "Rearrangement preserves orphan remnants around unrelated tools",
            vec![
                ev(A, cfc(&fc_lro, M)),
                ev(U, cfr(&fr_lro_inter, U)),
                ev(A, text("Still processing...", M)),
                ev(
                    U,
                    parts(
                        vec![Part {
                            function_response: Some(fr_orphaned.clone()),
                            ..ptext("first note")
                        }],
                        U,
                    ),
                ),
                ev(A, cfc(&fc_basic, M)),
                ev(U, cfr(&fr_basic, U)),
                ev(
                    U,
                    parts(
                        vec![Part {
                            inline_data: Some(image()),
                            function_response: Some(fr_orphaned.clone()),
                            ..Default::default()
                        }],
                        U,
                    ),
                ),
                ev(U, cfr(&fr_orphaned, U)),
                ev(U, cfr(&fr_lro_final, U)),
            ],
            vec![
                text("first note", U),
                cfc(&fc_basic, M),
                cfr(&fr_basic, U),
                parts(
                    vec![Part {
                        inline_data: Some(image()),
                        ..Default::default()
                    }],
                    U,
                ),
                cfc(&fc_lro, M),
                cfr(&fr_lro_final, U),
            ],
        ),
        case(
            "Mid-history mixed content preserves user text",
            vec![
                ev(U, text("Before", U)),
                ev(U, parts(vec![ptext("My question"), pfr(&fr_orphaned)], U)),
                ev(U, text("After", U)),
            ],
            vec![text("Before", U), text("My question", U), text("After", U)],
        ),
        case(
            "Orphaned function response is removed beside matching response",
            vec![
                ev(A, cfc(&fc_basic, M)),
                ev(U, parts(vec![pfr(&fr_basic), pfr(&fr_orphaned)], U)),
            ],
            vec![cfc(&fc_basic, M), cfr(&fr_basic, U)],
        ),
        case(
            "Only orphaned function response is dropped",
            vec![ev(U, cfr(&fr_orphaned, U))],
            vec![],
        ),
        case(
            "Consecutive orphaned function responses are dropped",
            vec![
                ev(U, text("Keep this", U)),
                ev(U, cfr(&fr_orphaned, U)),
                ev(U, cfr(&fr_orphaned, U)),
            ],
            vec![text("Keep this", U)],
        ),
        case(
            "Mid-history orphaned function response is dropped",
            vec![
                ev(U, text("Before", U)),
                ev(U, cfr(&fr_orphaned, U)),
                ev(U, text("After", U)),
            ],
            vec![text("Before", U), text("After", U)],
        ),
        case(
            "Function response without ID is preserved",
            vec![ev(U, cfr(&fr_without_id, U))],
            vec![cfr(&fr_without_id, U)],
        ),
        Case {
            want_err: Some("no function call event found"),
            ..case(
                "Function response with filtered matching call remains an error",
                vec![
                    ev(U, text("Regular message", U)),
                    Event {
                        isolation_scope: "task".into(),
                        ..ev(A, cfc(&fc_basic, M))
                    },
                    ev(U, cfr(&fr_basic, U)),
                ],
                vec![],
            )
        },
        Case {
            want_err: Some("no function call event found"),
            include_contents: IncludeContents::None,
            ..case(
                "Current turn retains validation for a call from an earlier turn",
                vec![
                    ev(A, cfc(&fc_basic, M)),
                    ev(U, text("Next question", U)),
                    ev(A, cfr(&fr_basic, U)),
                ],
                vec![],
            )
        },
        Case {
            want_err: Some(
                "validation failed: last response event has IDs not in the matching call event",
            ),
            ..case(
                "Responses for different function call events remain an error",
                vec![
                    ev(U, text("Run both", U)),
                    ev(A, cfc(&fc_basic, M)),
                    ev(A, cfc(&fc_other, M)),
                    ev(A, text("Working", M)),
                    ev(U, parts(vec![pfr(&fr_basic), pfr(&fr_other)], U)),
                ],
                vec![],
            )
        },
    ];

    for c in cases {
        let before = serde_json::to_string(&c.events).unwrap();
        let agent = llm_agent(LlmAgentConfig {
            name: A.into(),
            include_contents: c.include_contents,
            ..Default::default()
        });
        let ctx = ctx_builder(&agent, c.events.clone()).build_invocation();
        let result = run_contents(&ctx).await;
        let after = serde_json::to_string(&c.events).unwrap();
        assert_eq!(
            before, after,
            "{}: ContentsRequestProcessor mutated session events",
            c.name
        );
        match (c.want_err, result) {
            (Some(want_err), Err(e)) => assert!(
                e.to_string().contains(want_err),
                "{}: Expected error to contain {want_err:?}, got: {e}",
                c.name
            ),
            (Some(_), Ok(_)) => panic!(
                "{}: ContentsRequestProcessor succeeded; expected an error",
                c.name
            ),
            (None, Err(e)) => panic!("{}: ContentsRequestProcessor failed: {e}", c.name),
            (None, Ok(got)) => assert_eq!(with_continuation(c.want), got, "{}", c.name),
        }
    }
}
