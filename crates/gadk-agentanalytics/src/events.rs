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

//! Port of ADK-Go `plugin/agentanalytics/events.go`: tool-origin classification (`toolOriginOf`,
//! `resolveToolOrigin`, A2A detection, the HITL tool table, configured and resumed tool
//! origins) and the session-event signal rows (`STATE_DELTA`, `AGENT_TRANSFER`,
//! `EVENT_COMPACTION`, `AGENT_STATE_CHECKPOINT`, HITL / pause, `A2A_INTERACTION`, `NODE_ERROR`,
//! `NODE_OUTPUT`, `AGENT_RESPONSE`, user-message signals).
//!
//! Go's reflection over tool and agent values is gadk's typed accessors (R1): a tool's wrappers
//! are peeled with `Tool::unwrap` (at most [`MAX_UNWRAP_HOPS`] hops), its origin is
//! `Tool::origin_kind` and its agent `Tool::origin_agent`; an A2A agent is one whose
//! `agent_state().agent_type` is `AgentType::RemoteAgent`. Go's package / type-name heuristics
//! for user tools (names containing "skill" / "toolbox") are not reproduced: user tools report
//! their own origin. Divergence (ledger 7b-3a).

#![allow(dead_code, unused_variables)] // Task 6 implements the bodies.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use gadk_agent::{Agent, Tool};
use gadk_genai::{Content, Part};
use gadk_session::{Event, EventActions};
use serde_json::{Map, Value};

use crate::plugin::{AnalyticsPlugin, EventData};
use crate::redaction::CustomMetadataFilter;
use crate::trace::{CallbackCtx, Identity, RowSpan};

/// Go `toolOriginLocal`.
pub(crate) const TOOL_ORIGIN_LOCAL: &str = "LOCAL";
/// Go `toolOriginMCP`.
pub(crate) const TOOL_ORIGIN_MCP: &str = "MCP";
/// Go `toolOriginTransferAgent`.
pub(crate) const TOOL_ORIGIN_TRANSFER_AGENT: &str = "TRANSFER_AGENT";
/// Go `toolOriginTransferA2A`.
pub(crate) const TOOL_ORIGIN_TRANSFER_A2A: &str = "TRANSFER_A2A";
/// Go `toolOriginSubAgent`.
pub(crate) const TOOL_ORIGIN_SUB_AGENT: &str = "SUB_AGENT";
/// Go `toolOriginA2A`.
pub(crate) const TOOL_ORIGIN_A2A: &str = "A2A";
/// Go `toolOriginToolbox`.
pub(crate) const TOOL_ORIGIN_TOOLBOX: &str = "TOOLBOX";
/// Go `toolOriginSkill`.
pub(crate) const TOOL_ORIGIN_SKILL: &str = "SKILL";
/// Go `toolOriginUnknown`.
pub(crate) const TOOL_ORIGIN_UNKNOWN: &str = "UNKNOWN";

/// Go `hitlCredentialFunctionCallName` (private in gadk-engine's contents processor).
pub(crate) const HITL_CREDENTIAL_FUNCTION_CALL_NAME: &str = "adk_request_credential";

/// Go `maxUnwrapHops`.
pub(crate) const MAX_UNWRAP_HOPS: usize = 8;

/// Go `hitlToolMapping`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HitlToolMapping {
    pub(crate) request_event: &'static str,
    pub(crate) completed_event: &'static str,
    pub(crate) pause_kind: &'static str,
}

/// Go `hitlTools[name]`: the credential (`adk_request_credential`), confirmation
/// (`gadk_agent::REQUEST_CONFIRMATION_FUNCTION_CALL_NAME`) and workflow input
/// (`gadk_engine::workflow::request_input::WORKFLOW_INPUT_FUNCTION_CALL_NAME`) tools.
pub(crate) fn hitl_tool(name: &str) -> Option<HitlToolMapping> {
    todo!("Task 6")
}

/// Go `isModelResponseCode(code)`: a genai finish or blocked reason (trimmed).
pub(crate) fn is_model_response_code(code: &str) -> bool {
    todo!("Task 6")
}

/// Go `unwrapToolValue`: `t` with its wrappers peeled (`Tool::unwrap`, at most
/// [`MAX_UNWRAP_HOPS`] hops); `None` when the budget runs out (Go's invalid value).
pub(crate) fn unwrap_tool(t: &Arc<dyn Tool>) -> Option<Arc<dyn Tool>> {
    todo!("Task 6")
}

/// Go `toolOriginOf(t)`: the origin of the unwrapped tool from its `origin_kind()` (SubAgent →
/// [`sub_agent_or_a2a`] of its `origin_agent()`).
pub(crate) fn tool_origin_of(t: &Arc<dyn Tool>) -> &'static str {
    todo!("Task 6")
}

/// Go `subAgentOrA2A(v)`: A2A for a remote agent, else SUB_AGENT.
pub(crate) fn sub_agent_or_a2a(agent: Option<&Arc<dyn Agent>>) -> &'static str {
    todo!("Task 6")
}

/// Go `isRemoteA2AAgentValue(v)`: the agent's type is `AgentType::RemoteAgent`.
pub(crate) fn is_remote_a2a_agent(a: &dyn Agent) -> bool {
    todo!("Task 6")
}

/// Go `findAgentA2AByName(v, targetName, …)`: `(found, is_a2a)` for the agent named
/// `target_name` reachable from the tool's `origin_agent()` tree.
pub(crate) fn find_agent_a2a_by_name(t: &Arc<dyn Tool>, target_name: &str) -> (bool, bool) {
    todo!("Task 6")
}

/// Go `resolveToolOriginForCall(ctx, t, args)`: `tool_origin_of`, else TRANSFER_AGENT for a
/// tool named `transfer_to_agent`, else UNKNOWN.
pub(crate) fn resolve_tool_origin_for_call(
    t: &Arc<dyn Tool>,
    args: Option<&Map<String, Value>>,
) -> &'static str {
    todo!("Task 6")
}

/// Go `configuredToolOrigin(ctx, root, name)`: the origin of the tools named `name` configured
/// on the LLM agents of `root`'s tree (`gadk_engine::llm::llm_agent_config`), and whether
/// there are any.
pub(crate) fn configured_tool_origin(
    root: Option<&Arc<dyn Agent>>,
    name: &str,
) -> (&'static str, bool) {
    todo!("Task 6")
}

/// Go `scrubTempStateDelta(delta)`: without `temp:` keys; `None` when nothing is left.
pub(crate) fn scrub_temp_state_delta(delta: &Map<String, Value>) -> Option<Map<String, Value>> {
    todo!("Task 6")
}

/// Go `extractA2AMetadata(meta)`: the `a2a:` entries, or `None`.
pub(crate) fn extract_a2a_metadata(meta: &Map<String, Value>) -> Option<Map<String, Value>> {
    todo!("Task 6")
}

/// Go `extractAgentStateCheckpoint(meta)`: `Some((state, end_of_agent))` for a checkpoint.
pub(crate) fn extract_agent_state_checkpoint(meta: &Map<String, Value>) -> Option<(Value, bool)> {
    todo!("Task 6")
}

/// Go `epochSeconds(ts)`: seconds since the epoch as a float, or null for the zero time.
pub(crate) fn epoch_seconds(ts: Option<DateTime<Utc>>) -> Value {
    todo!("Task 6")
}

/// Go `hasActionsCompaction(actions)`.
pub(crate) fn has_actions_compaction(actions: &EventActions) -> bool {
    todo!("Task 6")
}

/// Go `extractEventCompaction(ev, cleanMeta, maxLen, redact)`: `(compaction, truncated,
/// ok)`.
pub(crate) fn extract_event_compaction(
    ev: &Event,
    clean_meta: Option<&Map<String, Value>>,
    max_len: i64,
    redact: bool,
) -> (Option<Map<String, Value>>, bool, bool) {
    todo!("Task 6")
}

/// Go `hasFunctionCall(content)`.
pub(crate) fn has_function_call(content: Option<&Content>) -> bool {
    todo!("Task 6")
}

/// Go `isLoggableFinalAgentResponse(ev)`.
pub(crate) fn is_loggable_final_agent_response(ev: &Event) -> bool {
    todo!("Task 6")
}

/// Go `isFinalResponse(ev)`.
pub(crate) fn is_final_response(ev: &Event) -> bool {
    todo!("Task 6")
}

/// Go `filterVisibleTextParts(parts)`: the non-thought parts with text.
pub(crate) fn filter_visible_text_parts(parts: &[Part]) -> Vec<Part> {
    todo!("Task 6")
}

/// Go `scrubA2ACustomMetadata(meta, allowlist, redact)`: `(kept entries, truncated)`.
pub(crate) fn scrub_a2a_custom_metadata(
    meta: &Map<String, Value>,
    allowlist: &CustomMetadataFilter,
    redact: bool,
) -> (Option<Map<String, Value>>, bool) {
    todo!("Task 6")
}

impl AnalyticsPlugin {
    /// Go `resolveToolOrigin(ctx, id, t, args)`: also tells a transfer to a remote agent
    /// (TRANSFER_A2A) through the tool's agents and the trace manager's `agent_named`.
    pub(crate) fn resolve_tool_origin(
        &self,
        ctx: &CallbackCtx,
        id: &Identity,
        t: &Arc<dyn Tool>,
        args: &Map<String, Value>,
    ) -> &'static str {
        todo!("Task 6")
    }

    /// Go `resumedToolOrigin(ctx, name)`: the origin of the tool a resumed function response
    /// answers, looked up in the runner agent's tree, then the highest known ancestor's.
    pub(crate) fn resumed_tool_origin(&self, ctx: &CallbackCtx, name: &str) -> &'static str {
        todo!("Task 6")
    }

    /// Go `emitUserMessageSignalEvents(ctx, id, msg, span)`: HITL completion and resumed
    /// `TOOL_COMPLETED` rows for the message's function responses (through
    /// `log_session_span_event`).
    pub(crate) fn emit_user_message_signal_events(
        &self,
        ctx: &CallbackCtx,
        id: &Identity,
        msg: &Content,
        span: &RowSpan,
    ) {
        todo!("Task 6")
    }

    /// Go `emitSessionEventSignals(ctx, ev)`: every signal row of a non-partial session event
    /// (through `log_session_event` / `log_session_span_event`; places from the trace
    /// manager's `event_span`).
    pub(crate) fn emit_session_event_signals(&self, ctx: &CallbackCtx, ev: &Event) {
        todo!("Task 6")
    }

    /// Go `emitHITLAndPauseSignals(ctx, id, ev, span, applyEventBase)`.
    pub(crate) fn emit_hitl_and_pause_signals(
        &self,
        ctx: &CallbackCtx,
        id: &Identity,
        ev: &Event,
        span: &RowSpan,
        apply_event_base: &dyn Fn(&mut EventData),
    ) {
        todo!("Task 6")
    }
}
