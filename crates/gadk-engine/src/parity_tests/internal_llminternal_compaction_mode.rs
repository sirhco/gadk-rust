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

//! Port of ADK-Go `internal/llminternal/compaction_mode_test.go`.

use gadk_agent::{InvocationContext, Mode};
use gadk_genai::{Content, ROLE_USER};

use super::internal_llminternal_contents_processor::llm_agent;
use crate::llm::LlmAgentConfig;
use crate::llm::mode::with_bound_mode;
use crate::llm::processors::compaction::prompt_token_estimator;

#[test]
fn test_prompt_token_estimator_uses_the_resolved_mode() {
    const AGENT_NAME: &str = "worker";
    let agent = llm_agent(LlmAgentConfig {
        name: AGENT_NAME.into(),
        ..Default::default()
    });
    let new_ctx = |bind: bool| -> InvocationContext {
        let ictx = InvocationContext::builder()
            .agent(agent.clone())
            .isolation_scope("scope-1")
            .user_content(Content::from_text("do the thing", ROLE_USER))
            .build_invocation();
        if bind {
            with_bound_mode(&ictx, AGENT_NAME, agent.agent_state(), Mode::SingleTurn)
        } else {
            ictx
        }
    };
    // Identical events and declaration on both sides; only the placement differs.
    let events: Vec<gadk_session::Event> = Vec::new();
    let placed = prompt_token_estimator(&new_ctx(true))(&events);
    let unplaced = prompt_token_estimator(&new_ctx(false))(&events);
    assert!(
        placed > unplaced,
        "estimate with a single_turn placement = {placed}, without one = {unplaced}; the placed \
         estimate must be the larger of the two, because its prompt carries the single-turn nudge \
         and the unplaced one does not"
    );
}
