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

//! A typed workflow node's output schema, bound to the invocation for the LLM agent it wraps.
//!
//! The schema is bound to the invocation rather than set on the agent because one agent can back
//! several nodes with different output types, and the agent's configuration is shared by all of
//! them. Like the bound mode ([`crate::llm::mode`]), the key is the agent's name and state
//! identity, so the binding reaches neither a peer the agent transfers to nor a same-named agent
//! nested beneath it.

use gadk_agent::{AgentState, ContextDelta, InvocationContext, Mode};
use gadk_genai::Schema;

use crate::llm::mode::mode_for;
use crate::llm::state::LlmAgentState;
use crate::llm::transfer::{should_use_auto_flow, transfer_target_agents};

/// `ctx` in which the agent identified by `agent_name` and
/// `state` is asked to answer with `schema`. `None` binds no schema, so an inner placement can
/// shadow an outer one.
pub(crate) fn with_bound_output_schema(
    ctx: &InvocationContext,
    agent_name: &str,
    state: &AgentState,
    schema: Option<Schema>,
) -> InvocationContext {
    ctx.with_delta(ContextDelta {
        bound_modes: Some(
            ctx.bound_modes()
                .with_output_schema(agent_name, state.key(), schema),
        ),
        ..Default::default()
    })
}

/// The output schema for the current agent's next request — its
/// declared `output_schema`, else a copy of the schema bound by [`with_bound_output_schema`],
/// else `None`. `state` is the current agent's.
///
/// A bound schema is a placement's request, not the agent's own, so it gives way whenever
/// applying it would change a request that works without it: the agent does not run single_turn
/// here; its `generate_content_config` sets a response schema, JSON schema or MIME type; or it
/// can transfer (the transfer_to_agent declaration is a function tool that
/// `need_output_schema_processor` does not count). The copy keeps a request callback that edits
/// the schema from reaching the node's other runs.
pub(crate) fn output_schema_for(ctx: &InvocationContext, state: &LlmAgentState) -> Option<Schema> {
    if let Some(declared) = &state.cfg.output_schema {
        return Some(declared.clone());
    }
    let cur = ctx.agent();
    let schema = ctx
        .bound_modes()
        .output_schema(cur.name(), cur.agent_state().key())
        .flatten()?;
    if mode_for(ctx, &*cur) != Mode::SingleTurn {
        return None;
    }
    if let Some(cfg) = &state.cfg.generate_content_config
        && (cfg.response_schema.is_some()
            || cfg.response_json_schema.is_some()
            || !cfg.response_mime_type.is_empty())
    {
        return None;
    }
    if should_use_auto_flow(&*cur) && !transfer_target_agents(ctx, &cur).is_empty() {
        return None;
    }
    Some(schema.clone())
}
