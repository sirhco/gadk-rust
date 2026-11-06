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

//! Port of Go internal/llminternal/mode_test.go.

use std::sync::Arc;

use gadk_agent::{AgentState, AgentType, InvocationContext, Mode};

use crate::llm::LlmAgentConfig;
use crate::llm::mode::{bound_mode_for, mode_for_state, resolve_mode, with_bound_mode};
use crate::llm::state::LlmAgentState;

/// Go `&State{Mode: mode}`: a distinct identity per call.
fn state(mode: Mode) -> AgentState {
    AgentState {
        agent_type: AgentType::LlmAgent,
        config: Some(Arc::new(LlmAgentState::new(LlmAgentConfig {
            mode,
            ..Default::default()
        }))),
        on_before_agent_callback_event: None,
    }
}

fn ctx() -> InvocationContext {
    InvocationContext::builder().build_invocation()
}

#[test]
fn test_resolve_mode() {
    let cases = [
        (
            "undeclared takes the placement",
            Mode::Unset,
            Mode::SingleTurn,
            Mode::SingleTurn,
        ),
        (
            "declaration beats the placement",
            Mode::Chat,
            Mode::SingleTurn,
            Mode::Chat,
        ),
        (
            "declared task beats a chat placement",
            Mode::Task,
            Mode::Chat,
            Mode::Task,
        ),
        (
            "no placement leaves it unset",
            Mode::Unset,
            Mode::Unset,
            Mode::Unset,
        ),
    ];
    for (name, declared, by_placement, want) in cases {
        assert_eq!(resolve_mode(declared, by_placement), want, "{name}");
    }
}

#[test]
fn test_bound_mode_scoping() {
    // a binding is invisible to another agent
    {
        let (worker, other) = (state(Mode::Unset), state(Mode::Chat));
        let c = with_bound_mode(&ctx(), "worker", &worker, Mode::SingleTurn);
        assert_eq!(bound_mode_for(&c, "other", &other), None);
        assert_eq!(mode_for_state(&c, "other", &other), Mode::Chat);
    }
    // binding one agent leaves another's alone
    {
        let (worker, helper) = (state(Mode::Unset), state(Mode::Unset));
        let c = with_bound_mode(&ctx(), "worker", &worker, Mode::SingleTurn);
        let c = with_bound_mode(&c, "helper", &helper, Mode::Chat);
        assert_eq!(
            bound_mode_for(&c, "worker", &worker),
            Some(Mode::SingleTurn)
        );
        assert_eq!(bound_mode_for(&c, "helper", &helper), Some(Mode::Chat));
    }
    // re-binding the same agent shadows the outer binding
    {
        let worker = state(Mode::Unset);
        let outer = with_bound_mode(&ctx(), "worker", &worker, Mode::Chat);
        let inner = with_bound_mode(&outer, "worker", &worker, Mode::SingleTurn);
        assert_eq!(
            bound_mode_for(&inner, "worker", &worker),
            Some(Mode::SingleTurn)
        );
        assert_eq!(bound_mode_for(&outer, "worker", &worker), Some(Mode::Chat));
    }
    // a binding does not reach a different agent of the same name
    {
        let (placed, nested) = (state(Mode::Unset), state(Mode::Unset));
        let c = with_bound_mode(&ctx(), "worker", &placed, Mode::SingleTurn);
        assert_eq!(bound_mode_for(&c, "worker", &nested), None);
        assert_eq!(mode_for_state(&c, "worker", &nested), Mode::Unset);
        assert_eq!(
            bound_mode_for(&c, "worker", &placed),
            Some(Mode::SingleTurn)
        );
    }
    // an unset mode does not bind
    {
        let worker = state(Mode::Task);
        let c = with_bound_mode(&ctx(), "worker", &worker, Mode::Unset);
        assert_eq!(bound_mode_for(&c, "worker", &worker), None);
        assert_eq!(mode_for_state(&c, "worker", &worker), Mode::Task);
    }
    // an empty name binds like any other
    {
        let (nameless, worker) = (state(Mode::Unset), state(Mode::Unset));
        let c = with_bound_mode(&ctx(), "", &nameless, Mode::SingleTurn);
        assert_eq!(bound_mode_for(&c, "", &nameless), Some(Mode::SingleTurn));
        assert_eq!(bound_mode_for(&c, "worker", &worker), None);
    }
    // two nameless agents do not share a binding
    {
        let (placed, nested) = (state(Mode::Unset), state(Mode::Unset));
        let c = with_bound_mode(&ctx(), "", &placed, Mode::SingleTurn);
        assert_eq!(bound_mode_for(&c, "", &nested), None);
    }
}

#[test]
fn test_mode_for() {
    // the binding supplies a mode the agent did not declare
    let worker = state(Mode::Unset);
    let c = with_bound_mode(&ctx(), "worker", &worker, Mode::SingleTurn);
    assert_eq!(mode_for_state(&c, "worker", &worker), Mode::SingleTurn);
    // the declaration is the fallback
    assert_eq!(
        mode_for_state(&ctx(), "worker", &state(Mode::Chat)),
        Mode::Chat
    );
    // the binding wins for the agent it was resolved for
    let worker = state(Mode::Chat);
    let c = with_bound_mode(&ctx(), "worker", &worker, Mode::SingleTurn);
    assert_eq!(mode_for_state(&c, "worker", &worker), Mode::SingleTurn);
    // another agent's binding loses to this agent's declaration
    let placed = state(Mode::Unset);
    for (declared, bound) in [
        (Mode::Unset, Mode::SingleTurn),
        (Mode::Chat, Mode::SingleTurn),
        (Mode::SingleTurn, Mode::Task),
        (Mode::Task, Mode::SingleTurn),
    ] {
        let nested = state(declared);
        let c = with_bound_mode(&ctx(), "worker", &placed, bound);
        assert_eq!(
            mode_for_state(&c, "worker", &nested),
            declared,
            "declared {declared:?} bound {bound:?}"
        );
    }
    // a same-named agent's binding does not displace ours
    let (ours, theirs) = (state(Mode::Unset), state(Mode::Unset));
    let c = with_bound_mode(&ctx(), "worker", &ours, Mode::SingleTurn);
    let c = with_bound_mode(&c, "worker", &theirs, Mode::Task);
    assert_eq!(bound_mode_for(&c, "worker", &ours), Some(Mode::SingleTurn));
    assert_eq!(bound_mode_for(&c, "worker", &theirs), Some(Mode::Task));
}
