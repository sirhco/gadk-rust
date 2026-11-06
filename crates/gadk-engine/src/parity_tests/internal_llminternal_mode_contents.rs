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

//! Port of ADK-Go `internal/llminternal/mode_contents_test.go`.

use std::sync::Arc;

use gadk_agent::{Agent, Mode};
use gadk_genai::{Content, ROLE_MODEL, ROLE_USER};
use gadk_session::Event;

use super::internal_llminternal_contents_processor::{
    ctx_builder, ev, llm_agent, run_contents, text,
};
use crate::llm::LlmAgentConfig;
use crate::llm::mode::with_bound_mode;
use crate::llm::processors::contents::SINGLE_TURN_NUDGE;

fn has_text(contents: &[Content], t: &str) -> bool {
    contents
        .iter()
        .flat_map(|c| c.parts.iter())
        .any(|p| p.text == t)
}

/// The three-event history every test uses: the agent's own reply sits between the two user turns.
fn history(agent_reply_author: &str) -> Vec<Event> {
    vec![
        ev("user", text("older turn", ROLE_USER)),
        ev(agent_reply_author, text("older answer", ROLE_MODEL)),
        ev("user", text("current turn", ROLE_USER)),
    ]
}

#[tokio::test]
async fn test_contents_request_processor_honours_resolved_mode_over_declaration() {
    const AGENT_NAME: &str = "worker";
    let cases = [
        (
            "single_turn placement drops history",
            Mode::SingleTurn,
            false,
        ),
        ("chat placement keeps history", Mode::Chat, true),
    ];
    for (name, resolved, want_history) in cases {
        let agent: Arc<dyn Agent> = llm_agent(LlmAgentConfig {
            name: AGENT_NAME.into(),
            ..Default::default()
        });
        let ctx = ctx_builder(&agent, history(AGENT_NAME)).build_invocation();
        let ctx = with_bound_mode(&ctx, AGENT_NAME, agent.agent_state(), resolved);
        let got = run_contents(&ctx)
            .await
            .unwrap_or_else(|e| panic!("{name}: ContentsRequestProcessor: {e}"));
        assert_eq!(
            has_text(&got, "older turn"),
            want_history,
            "{name}: history present mismatch (contents: {got:?})"
        );
    }
}

#[tokio::test]
async fn test_contents_request_processor_bound_mode_is_scoped_to_its_agent() {
    let agent = llm_agent(LlmAgentConfig {
        name: "other".into(),
        ..Default::default()
    });
    let ctx = ctx_builder(&agent, history("other")).build_invocation();
    // single_turn was resolved under the name "worker", not the name of the agent running here.
    // The IDENTITY is this agent's, so the name is the only thing that differs.
    let ctx = with_bound_mode(&ctx, "worker", agent.agent_state(), Mode::SingleTurn);
    let got = run_contents(&ctx).await.unwrap();
    assert!(
        has_text(&got, "older turn"),
        "a mode bound for another agent suppressed this agent's history; contents = {got:?}"
    );
}

#[tokio::test]
async fn test_contents_request_processor_declared_single_turn_without_placement_keeps_history() {
    let agent = llm_agent(LlmAgentConfig {
        name: "worker".into(),
        mode: Mode::SingleTurn,
        ..Default::default()
    });
    let ctx = ctx_builder(&agent, history("worker")).build_invocation();
    let got = run_contents(&ctx)
        .await
        .unwrap_or_else(|e| panic!("ContentsRequestProcessor: {e}"));
    assert!(
        has_text(&got, "older turn"),
        "declared single_turn outside a placement lost its history; contents = {got:?}"
    );
}

#[tokio::test]
async fn test_contents_request_processor_resolved_single_turn_gets_the_nudge() {
    const AGENT_NAME: &str = "worker";
    // (name, declared, bind, want)
    let cases = [
        ("placement resolves it", Mode::Unset, true, true),
        ("declaration alone", Mode::SingleTurn, false, true),
        ("neither", Mode::Unset, false, false),
    ];
    for (name, declared, bind, want) in cases {
        let agent = llm_agent(LlmAgentConfig {
            name: AGENT_NAME.into(),
            mode: declared,
            ..Default::default()
        });
        let ctx = ctx_builder(&agent, vec![])
            .isolation_scope("scope-1")
            .user_content(text("do the thing", ROLE_USER))
            .build_invocation();
        let ctx = if bind {
            with_bound_mode(&ctx, AGENT_NAME, agent.agent_state(), Mode::SingleTurn)
        } else {
            ctx
        };
        let got = run_contents(&ctx)
            .await
            .unwrap_or_else(|e| panic!("{name}: ContentsRequestProcessor: {e}"));
        assert_eq!(
            has_text(&got, SINGLE_TURN_NUDGE),
            want,
            "{name}: single-turn nudge present mismatch (contents: {got:?})"
        );
    }
}

#[tokio::test]
async fn test_contents_request_processor_a_foreign_single_turn_binding_does_not_hide_a_chat_agents_history()
 {
    let agent = llm_agent(LlmAgentConfig {
        name: "worker".into(),
        mode: Mode::Chat,
        ..Default::default()
    });
    // single_turn resolved for a DIFFERENT agent that happens to be called "worker" too. Go binds
    // a fresh &llminternal.State{}; a second agent's AgentState is equally distinct.
    let decoy = llm_agent(LlmAgentConfig {
        name: "worker".into(),
        ..Default::default()
    });
    let ctx = ctx_builder(&agent, history("worker")).build_invocation();
    let ctx = with_bound_mode(&ctx, "worker", decoy.agent_state(), Mode::SingleTurn);
    let got = run_contents(&ctx)
        .await
        .unwrap_or_else(|e| panic!("ContentsRequestProcessor: {e}"));
    assert!(
        !got.is_empty(),
        "no contents were built, so this test pins nothing"
    );
    assert!(
        has_text(&got, "older turn"),
        "a single_turn placement resolved for another agent hid this chat agent's history; contents = {got:?}"
    );
}
