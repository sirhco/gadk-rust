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

//! Mode resolution (port of Go `internal/llminternal/mode.go`; `is_untransferable_mode` is Go
//! `agent_transfer.go` `isUntransferableMode`).
//!
//! An agent that declares no mode takes one from where it is placed: chat at a runner root,
//! single_turn at a workflow node. The declaration (`LlmAgentConfig::mode`) is immutable, so the
//! resolved mode lives on the invocation as a binding instead: one agent instance serves many
//! concurrent invocations and may sit at two placements.
//!
//! Rules:
//! - Readers that only test for task mode stay on the declaration: no placement defaults to task.
//! - `is_untransferable_mode` and task-tool installation read the declaration: they decide the
//!   shape of the agent tree, which a per-run placement cannot answer.
//! - The history half of the contents processor reads [`bound_mode`] directly, to tell a
//!   single_turn placement from a single_turn declaration; every other reader resolves through
//!   [`mode_for`].
//! - A binding is keyed by (agent name, `AgentState::key()`), so it never governs an agent it was
//!   not resolved for, survives nested activations that bind other agents, and keeps two distinct
//!   same-named agents in separate slots. Re-binding the same agent shadows the outer binding.
//! - An empty name binds like any other; binding `Mode::Unset` is a no-op. There is no way to clear
//!   a binding.

use gadk_agent::{Agent, AgentState, ContextDelta, InvocationContext, Mode};

use crate::llm::state::{reveal, reveal_state};

/// Port of Go ResolveMode: `declared` when set, else `by_placement`.
pub(crate) fn resolve_mode(declared: Mode, by_placement: Mode) -> Mode {
    if declared == Mode::Unset {
        by_placement
    } else {
        declared
    }
}

/// Port of Go WithBoundMode: `ctx` carrying the mode the named agent runs under for this invocation.
/// `Mode::Unset` returns `ctx` unchanged (a clone).
pub(crate) fn with_bound_mode(
    ctx: &InvocationContext,
    agent_name: &str,
    state: &AgentState,
    mode: Mode,
) -> InvocationContext {
    if mode == Mode::Unset {
        return ctx.clone();
    }
    ctx.with_delta(ContextDelta {
        bound_modes: Some(ctx.bound_modes().with(agent_name, state.key(), mode)),
        ..Default::default()
    })
}

/// Port of Go BoundMode on an explicit (name, state) identity.
pub(crate) fn bound_mode_for(
    ctx: &InvocationContext,
    agent_name: &str,
    state: &AgentState,
) -> Option<Mode> {
    ctx.bound_modes().get(agent_name, state.key())
}

/// Port of Go ModeFor on an explicit identity: the binding, else the declaration (`Unset` for
/// non-LLM agents).
pub(crate) fn mode_for_state(
    ctx: &InvocationContext,
    agent_name: &str,
    state: &AgentState,
) -> Mode {
    bound_mode_for(ctx, agent_name, state)
        .unwrap_or_else(|| reveal_state(state).map_or(Mode::Unset, |s| s.cfg.mode))
}

/// Port of Go BoundMode: the mode bound to `agent` in `ctx`, if any.
pub(crate) fn bound_mode(ctx: &InvocationContext, agent: &dyn Agent) -> Option<Mode> {
    bound_mode_for(ctx, agent.name(), agent.agent_state())
}

/// Port of Go ModeFor: the mode `agent` runs under in `ctx`.
pub(crate) fn mode_for(ctx: &InvocationContext, agent: &dyn Agent) -> Mode {
    mode_for_state(ctx, agent.name(), agent.agent_state())
}

/// Port of Go isUntransferableMode: reads the declaration only; `ctx` is unused, as in Go.
pub(crate) fn is_untransferable_mode(_ctx: &InvocationContext, agent: &dyn Agent) -> bool {
    matches!(
        reveal(agent).map(|s| s.cfg.mode),
        Some(Mode::Task | Mode::SingleTurn)
    )
}
