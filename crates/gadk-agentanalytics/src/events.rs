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
//! `Tool::origin_kind`, its agent `Tool::origin_agent` and every agent it can hand a call to
//! `Tool::origin_agents` (a transfer tool's supported agents); an A2A agent is one whose
//! `agent_state().agent_type` is `AgentType::RemoteAgent`. Go's package / type-name heuristics
//! for user tools (names containing "skill" / "toolbox") are not reproduced: user tools report
//! their own origin (the divergence is annotated in [`tool_origin_of`]).

use std::collections::HashSet;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use gadk_agent::{Agent, AgentType, REQUEST_CONFIRMATION_FUNCTION_CALL_NAME, Tool, ToolOriginKind};
use gadk_engine::workflow::WORKFLOW_INPUT_FUNCTION_CALL_NAME;
use gadk_genai::{Content, FunctionCall, Part};
use gadk_session::{Event, EventActions, EventCompaction, KEY_PREFIX_TEMP, zero_time};
use serde_json::{Map, Value};

use crate::event_types::{
    EVENT_A2A_INTERACTION, EVENT_AGENT_RESPONSE, EVENT_AGENT_STATE_CHECKPOINT,
    EVENT_AGENT_TRANSFER, EVENT_EVENT_COMPACTION, EVENT_HITL_CONFIRMATION_COMPLETED,
    EVENT_HITL_CONFIRMATION_REQUEST, EVENT_HITL_CREDENTIAL_COMPLETED,
    EVENT_HITL_CREDENTIAL_REQUEST, EVENT_HITL_INPUT_COMPLETED, EVENT_HITL_INPUT_REQUEST,
    EVENT_NODE_ERROR, EVENT_NODE_OUTPUT, EVENT_STATE_DELTA, EVENT_TOOL_COMPLETED,
    EVENT_TOOL_PAUSED,
};
use crate::formatter::{
    NodeBudget, format_content_parts_with, format_response_content, sanitize_and_truncate_text,
};
use crate::plugin::rows::{identity_of, put_non_empty, runner_agent_of};
use crate::plugin::{AnalyticsPlugin, EventData, Payload, USER_AUTHOR};
use crate::redaction::{
    COLUMN_CONTENT_PARTS, CustomMetadataFilter, REDACTED_VALUE, is_sensitive_key, redact_json_value,
};
use crate::trace::{AgentKey, CallbackCtx, Identity, RowSpan};

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

/// The name of the transfer tool (Go's literal in `resolveToolOriginForCall`).
const TRANSFER_TO_AGENT_TOOL_NAME: &str = "transfer_to_agent";

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
    match name {
        HITL_CREDENTIAL_FUNCTION_CALL_NAME => Some(HitlToolMapping {
            request_event: EVENT_HITL_CREDENTIAL_REQUEST,
            completed_event: EVENT_HITL_CREDENTIAL_COMPLETED,
            pause_kind: "hitl_credential",
        }),
        REQUEST_CONFIRMATION_FUNCTION_CALL_NAME => Some(HitlToolMapping {
            request_event: EVENT_HITL_CONFIRMATION_REQUEST,
            completed_event: EVENT_HITL_CONFIRMATION_COMPLETED,
            pause_kind: "hitl_confirmation",
        }),
        WORKFLOW_INPUT_FUNCTION_CALL_NAME => Some(HitlToolMapping {
            request_event: EVENT_HITL_INPUT_REQUEST,
            completed_event: EVENT_HITL_INPUT_COMPLETED,
            pause_kind: "hitl_input",
        }),
        _ => None,
    }
}

/// Go `modelResponseCodes`: the genai finish and blocked reasons Go's genai version lists (its
/// set, not gadk-genai's: `TOO_MANY_TOOL_CALLS` and `CONTINUATION` are not in it).
const MODEL_RESPONSE_CODES: [&str; 25] = [
    "FINISH_REASON_UNSPECIFIED",
    "STOP",
    "MAX_TOKENS",
    "SAFETY",
    "RECITATION",
    "LANGUAGE",
    "OTHER",
    "BLOCKLIST",
    "PROHIBITED_CONTENT",
    "SPII",
    "MALFORMED_FUNCTION_CALL",
    "IMAGE_SAFETY",
    "UNEXPECTED_TOOL_CALL",
    "IMAGE_PROHIBITED_CONTENT",
    "NO_IMAGE",
    "IMAGE_RECITATION",
    "IMAGE_OTHER",
    "BLOCKED_REASON_UNSPECIFIED",
    "SAFETY",
    "OTHER",
    "BLOCKLIST",
    "PROHIBITED_CONTENT",
    "IMAGE_SAFETY",
    "MODEL_ARMOR",
    "JAILBREAK",
];

/// Go `isModelResponseCode(code)`: a genai finish or blocked reason (trimmed).
pub(crate) fn is_model_response_code(code: &str) -> bool {
    MODEL_RESPONSE_CODES.contains(&code.trim())
}

/// Go `unwrapToolValue`: `t` with its wrappers peeled (`Tool::unwrap`, at most
/// [`MAX_UNWRAP_HOPS`] hops); `None` when the budget runs out (Go's invalid value).
///
/// Go spends one hop per `Unwrap()` and stops at the first value that does not unwrap, so a
/// tool under at most `MAX_UNWRAP_HOPS - 1` wrappers resolves; a cycle exhausts the budget
/// (Go's visited-pointer check returns the same invalid value earlier).
pub(crate) fn unwrap_tool(t: &Arc<dyn Tool>) -> Option<Arc<dyn Tool>> {
    let mut cur = Arc::clone(t);
    for _ in 0..MAX_UNWRAP_HOPS {
        match cur.unwrap() {
            Some(inner) => cur = inner,
            None => return Some(cur),
        }
    }
    None
}

/// Go `toolOriginOf(t)`: the origin of the unwrapped tool from its `origin_kind()` (SubAgent →
/// [`sub_agent_or_a2a`] of its `origin_agent()`).
pub(crate) fn tool_origin_of(t: &Arc<dyn Tool>) -> &'static str {
    let Some(u) = unwrap_tool(t) else {
        return TOOL_ORIGIN_UNKNOWN;
    };
    // Divergence (ledger 7b-3a): Go's `classifyADKToolType` classifies by the tool's package
    // path and type name (and calls any user tool whose package or type name contains
    // "toolbox" / "skill" TOOLBOX / SKILL); gadk tools declare their kind through
    // `Tool::origin_kind`, and a user tool that declares none is UNKNOWN (spec R1(b)).
    match u.origin_kind() {
        ToolOriginKind::Unknown => TOOL_ORIGIN_UNKNOWN,
        ToolOriginKind::Local => TOOL_ORIGIN_LOCAL,
        ToolOriginKind::Mcp => TOOL_ORIGIN_MCP,
        ToolOriginKind::TransferAgent => TOOL_ORIGIN_TRANSFER_AGENT,
        ToolOriginKind::SubAgent => sub_agent_or_a2a(u.origin_agent().as_ref()),
        ToolOriginKind::Skill => TOOL_ORIGIN_SKILL,
        ToolOriginKind::Toolbox => TOOL_ORIGIN_TOOLBOX,
    }
}

/// Go `subAgentOrA2A(v)`: A2A for a remote agent, else SUB_AGENT.
pub(crate) fn sub_agent_or_a2a(agent: Option<&Arc<dyn Agent>>) -> &'static str {
    match agent {
        Some(a) if is_remote_a2a_agent(a.as_ref()) => TOOL_ORIGIN_A2A,
        _ => TOOL_ORIGIN_SUB_AGENT,
    }
}

/// Go `isRemoteA2AAgentValue(v)`: the agent's type is `AgentType::RemoteAgent`.
pub(crate) fn is_remote_a2a_agent(a: &dyn Agent) -> bool {
    a.agent_state().agent_type == AgentType::RemoteAgent
}

/// Go `findAgentA2AByName(v, targetName, …)`: `(found, is_a2a)` for the agent named
/// `target_name` reachable from the unwrapped tool's agents.
///
/// Go walks the unwrapped tool's agent fields (`agent`, `supportedAgents`, …) by reflection; a
/// gadk tool exposes them through `Tool::origin_agents()` (an agent tool's agent, the transfer
/// tool's supported agents), searched in order: the agent itself by name, then its tree
/// (`find_agent`). A panicking `name()`/`find_agent()` reads as no match (Go's `orZero`).
// Divergence (ledger 7b-3a): Go reflects over the unwrapped tool's `agent` / `supportedAgents` / `SubAgents` / `RootAgent` fields; gadk searches the agents the tool exposes through the typed accessor `Tool::origin_agents()` (R1), so a tool that does not override it is searched through `origin_agent()` only.
pub(crate) fn find_agent_a2a_by_name(t: &Arc<dyn Tool>, target_name: &str) -> (bool, bool) {
    let Some(u) = unwrap_tool(t) else {
        return (false, false);
    };
    for agent in u.origin_agents() {
        let name = catch_unwind(AssertUnwindSafe(|| agent.name().trim() == target_name));
        if name.unwrap_or(false) {
            return (true, is_remote_a2a_agent(agent.as_ref()));
        }
        let found = catch_unwind(AssertUnwindSafe(|| agent.find_agent(target_name)));
        if let Ok(Some(found)) = found {
            return (true, is_remote_a2a_agent(found.as_ref()));
        }
    }
    (false, false)
}

/// Go `resolveToolOriginForCall(ctx, t, args)`: `tool_origin_of`, else TRANSFER_AGENT for a
/// tool named `transfer_to_agent`, else UNKNOWN. A panicking tool is UNKNOWN (Go's
/// `recover`).
pub(crate) fn resolve_tool_origin_for_call(
    t: &Arc<dyn Tool>,
    _args: Option<&Map<String, Value>>,
) -> &'static str {
    catch_unwind(AssertUnwindSafe(|| {
        let origin = tool_origin_of(t);
        if origin != TOOL_ORIGIN_UNKNOWN {
            return origin;
        }
        // The name is checked only after the kind, so a function tool named
        // transfer_to_agent is LOCAL.
        if t.name().trim() == TRANSFER_TO_AGENT_TOOL_NAME {
            return TOOL_ORIGIN_TRANSFER_AGENT;
        }
        TOOL_ORIGIN_UNKNOWN
    }))
    .unwrap_or(TOOL_ORIGIN_UNKNOWN)
}

/// Go `configuredToolOrigin(ctx, root, name)`: the origin of the tools named `name` configured
/// on the LLM agents of `root`'s tree (`gadk_engine::llm::llm_agent_config`), and whether
/// there are any. Tools of different origins yield UNKNOWN; no such tool yields `("", false)`
/// (Go's zero string).
pub(crate) fn configured_tool_origin(
    root: Option<&Arc<dyn Agent>>,
    name: &str,
) -> (&'static str, bool) {
    fn walk(
        a: &Arc<dyn Agent>,
        name: &str,
        visited: &mut HashSet<AgentKey>,
        origin: &mut &'static str,
        found: &mut bool,
    ) {
        if !visited.insert(AgentKey::of(a)) {
            return;
        }
        if let Some(cfg) = gadk_engine::llm::llm_agent_config(a.as_ref()) {
            for t in cfg.tools.iter().filter(|t| t.name() == name) {
                let mut o = resolve_tool_origin_for_call(t, None);
                if *found && o != *origin {
                    o = TOOL_ORIGIN_UNKNOWN;
                }
                *origin = o;
                *found = true;
            }
        }
        for sub in a.sub_agents() {
            walk(sub, name, visited, origin, found);
        }
    }
    let mut origin = "";
    let mut found = false;
    if let Some(root) = root {
        walk(root, name, &mut HashSet::new(), &mut origin, &mut found);
    }
    (origin, found)
}

/// Go `scrubTempStateDelta(delta)`: without `temp:` keys; `None` when nothing is left.
pub(crate) fn scrub_temp_state_delta(delta: &Map<String, Value>) -> Option<Map<String, Value>> {
    let out: Map<String, Value> = delta
        .iter()
        .filter(|(k, _)| !k.trim().starts_with(KEY_PREFIX_TEMP))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    (!out.is_empty()).then_some(out)
}

/// Go `extractA2AMetadata(meta)`: the `a2a:` entries, or `None`.
pub(crate) fn extract_a2a_metadata(meta: &Map<String, Value>) -> Option<Map<String, Value>> {
    let out: Map<String, Value> = meta
        .iter()
        .filter(|(k, _)| k.trim().to_lowercase().starts_with("a2a:"))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    (!out.is_empty()).then_some(out)
}

/// Go `extractAgentStateCheckpoint(meta)`: `Some((state, end_of_agent))` for a checkpoint.
/// A terminal checkpoint (`end_of_agent` true) is kept without a state (`Value::Null`).
pub(crate) fn extract_agent_state_checkpoint(meta: &Map<String, Value>) -> Option<(Value, bool)> {
    if meta.is_empty() {
        return None;
    }
    let end_of_agent = matches!(meta.get("end_of_agent"), Some(Value::Bool(true)))
        || matches!(meta.get("adk_end_of_agent"), Some(Value::Bool(true)));
    let mut state = meta.get("adk_agent_state").filter(|v| !v.is_null());
    if state.is_none()
        && let Some(alt) = meta.get("agent_state")
    {
        state = Some(alt);
    }
    let state = state.cloned().unwrap_or(Value::Null);
    if state.is_null() && !end_of_agent {
        return None;
    }
    Some((state, end_of_agent))
}

/// Go `epochSeconds(ts)`: seconds since the epoch as a float, or null for the zero time
/// (`None`, or gadk-session's `zero_time()`).
pub(crate) fn epoch_seconds(ts: Option<DateTime<Utc>>) -> Value {
    match ts {
        Some(ts) if ts != zero_time() => {
            Value::from(ts.timestamp() as f64 + f64::from(ts.timestamp_subsec_nanos()) / 1e9)
        }
        _ => Value::Null,
    }
}

/// Go `hasActionsCompaction(actions)`.
pub(crate) fn has_actions_compaction(actions: &EventActions) -> bool {
    actions.compaction.is_some()
}

/// Go `extractEventCompaction(ev, cleanMeta, maxLen, redact)`: `(compaction, truncated,
/// ok)`, from `actions.compaction`, else from the scrubbed custom metadata's
/// `adk_compaction` entry.
pub(crate) fn extract_event_compaction(
    ev: &Event,
    clean_meta: Option<&Map<String, Value>>,
    max_len: i64,
    redact: bool,
) -> (Option<Map<String, Value>>, bool, bool) {
    if let Some(c) = &ev.actions.compaction
        && let Some((payload, trunc)) = format_actions_compaction(c, max_len, redact)
    {
        return (Some(payload), trunc, true);
    }
    if let Some(raw) = clean_meta.and_then(|m| m.get("adk_compaction"))
        && let Some((payload, trunc)) = format_compaction_value(raw, max_len, redact)
    {
        return (Some(payload), trunc, true);
    }
    (None, false, false)
}

/// Go `formatCompactionValue` over the typed `session.EventCompaction` (its struct branch).
fn format_actions_compaction(
    c: &EventCompaction,
    max_len: i64,
    redact: bool,
) -> Option<(Map<String, Value>, bool)> {
    let start = epoch_seconds(Some(c.start_timestamp));
    let end = epoch_seconds(Some(c.end_timestamp));
    // Go's `case *genai.Content` matches a nil pointer too.
    let (summary, trunc) =
        scrub_compaction_genai_content(c.compacted_content.as_ref(), max_len, redact);
    if start.is_null() && end.is_null() && summary.is_empty() {
        return None;
    }
    Some((compaction_payload(start, end, summary), trunc))
}

/// Go `formatCompactionValue` over a custom-metadata value (its map branch; gadk custom
/// metadata is JSON, so Go's struct branch for metadata values does not arise).
fn format_compaction_value(
    v: &Value,
    max_len: i64,
    redact: bool,
) -> Option<(Map<String, Value>, bool)> {
    let Value::Object(m) = v else {
        return None;
    };
    let mut out = m.clone();
    normalize_compaction_keys(&mut out);
    if !out.contains_key("start_timestamp")
        && !out.contains_key("end_timestamp")
        && !out.contains_key("compacted_content")
    {
        return None;
    }
    let start = compaction_timestamp_value(out.get("start_timestamp"));
    let end = compaction_timestamp_value(out.get("end_timestamp"));
    let (summary, trunc) = match out.get("compacted_content") {
        Some(Value::String(s)) => sanitize_and_truncate_text(s, max_len, redact),
        Some(Value::Object(c)) => match genai_content_from_map(c) {
            Some(gc) => scrub_compaction_genai_content(Some(&gc), max_len, redact),
            None => (String::new(), false),
        },
        _ => (String::new(), false),
    };
    Some((compaction_payload(start, end, summary), trunc))
}

/// The `EVENT_COMPACTION` payload.
fn compaction_payload(start: Value, end: Value, summary: String) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("start_timestamp".to_owned(), start);
    m.insert("end_timestamp".to_owned(), end);
    m.insert("compacted_content".to_owned(), Value::String(summary));
    m
}

/// Go `scrubCompactionGenAIContent(c, maxLen, redact)`.
fn scrub_compaction_genai_content(
    c: Option<&Content>,
    max_len: i64,
    redact: bool,
) -> (String, bool) {
    let (summary, trunc) = format_response_content(c, max_len, redact);
    if !redact || summary.is_empty() {
        return (summary, trunc);
    }
    let (scrubbed, s_trunc) = sanitize_and_truncate_text(&summary, max_len, redact);
    (scrubbed, trunc || s_trunc)
}

/// Go `compactionTimestampValue(v)` / `compactionTimestamp(f)` over a JSON value: a number is
/// epoch seconds already and kept as is, an RFC 3339 string is converted; zero, empty or
/// unparseable values are null.
fn compaction_timestamp_value(v: Option<&Value>) -> Value {
    match v {
        Some(Value::Number(n)) if n.as_f64() != Some(0.0) => Value::Number(n.clone()),
        Some(Value::String(s)) if !s.is_empty() => match DateTime::parse_from_rfc3339(s) {
            Ok(ts) => epoch_seconds(Some(ts.with_timezone(&Utc))),
            Err(_) => Value::Null,
        },
        _ => Value::Null,
    }
}

/// Go `compactionKeyAliases`: for each payload key, its Go field name and its
/// `session.EventCompaction` JSON name.
const COMPACTION_KEY_ALIASES: [(&str, [&str; 2]); 3] = [
    ("start_timestamp", ["StartTimestamp", "startTimestamp"]),
    ("end_timestamp", ["EndTimestamp", "endTimestamp"]),
    (
        "compacted_content",
        ["CompactedContent", "compactedContent"],
    ),
];

/// Go `normalizeCompactionKeys(m)`: renames alias keys to their payload keys; an existing
/// payload key wins over its aliases; aliases are always removed.
fn normalize_compaction_keys(m: &mut Map<String, Value>) {
    for (key, aliases) in COMPACTION_KEY_ALIASES {
        for alias in aliases {
            if let Some(v) = m.remove(alias)
                && !m.contains_key(key)
            {
                m.insert(key.to_owned(), v);
            }
        }
    }
}

/// Go `genaiContentFromMap(m)`: the content a map decodes as, if it has parts.
fn genai_content_from_map(m: &Map<String, Value>) -> Option<Content> {
    if !m.contains_key("parts") {
        return None;
    }
    serde_json::from_value::<Content>(Value::Object(m.clone()))
        .ok()
        .filter(|c| !c.parts.is_empty())
}

/// Go `hasFunctionCall(content)`.
pub(crate) fn has_function_call(content: Option<&Content>) -> bool {
    content.is_some_and(|c| c.parts.iter().any(|p| p.function_call.is_some()))
}

/// Go `isLoggableFinalAgentResponse(ev)`: a final agent turn that may emit `AGENT_RESPONSE`
/// once its visible text parts are filtered (skip-summarization events included; partial,
/// user-authored, compaction, long-running and function-call events excluded).
pub(crate) fn is_loggable_final_agent_response(ev: &Event) -> bool {
    let Some(content) = ev.response.content.as_ref() else {
        return false;
    };
    if ev.response.partial || ev.author == USER_AUTHOR || content.parts.is_empty() {
        return false;
    }
    if !ev.long_running_tool_ids.is_empty()
        || has_function_call(Some(content))
        || has_actions_compaction(&ev.actions)
    {
        return false;
    }
    is_final_response(ev) || ev.actions.skip_summarization
}

/// Go `isFinalResponse(ev)`: `false` without content, else `Event::is_final_response` (gadk
/// parts are never nil, so Go's nil-part filtering has nothing to drop).
pub(crate) fn is_final_response(ev: &Event) -> bool {
    ev.response.content.is_some() && ev.is_final_response()
}

/// Go `filterVisibleTextParts(parts)`: the non-thought parts with text.
pub(crate) fn filter_visible_text_parts(parts: &[Part]) -> Vec<Part> {
    parts
        .iter()
        .filter(|p| !p.thought && !p.text.is_empty())
        .cloned()
        .collect()
}

/// Go `scrubA2ACustomMetadata(meta, allowlist, redact)`: `(kept entries, truncated)`. Only
/// allowlisted keys and `a2a:` keys are kept; with `redact`, a sensitive key's value (the key
/// with or without its `a2a:` prefix) is `[REDACTED]` and every other value goes through the
/// key-based JSON redaction.
pub(crate) fn scrub_a2a_custom_metadata(
    meta: &Map<String, Value>,
    allowlist: &CustomMetadataFilter,
    redact: bool,
) -> (Option<Map<String, Value>>, bool) {
    let mut out = Map::new();
    let mut trunc = false;
    for (k, v) in meta {
        if !allowlist.allows(k) && !k.starts_with("a2a:") {
            continue;
        }
        if !redact {
            out.insert(k.clone(), v.clone());
            continue;
        }
        if is_sensitive_key(k) || is_sensitive_key(k.strip_prefix("a2a:").unwrap_or(k)) {
            out.insert(k.clone(), Value::String(REDACTED_VALUE.to_owned()));
            continue;
        }
        let mut budget = NodeBudget::new();
        let (clean, _, lost) = redact_json_value(v, 0, &mut budget);
        out.insert(k.clone(), clean);
        trunc = trunc || lost;
    }
    if out.is_empty() {
        return (None, false);
    }
    (Some(out), trunc)
}

/// Go `nonNilMap(m)` as a JSON value.
fn json_map(m: &Map<String, Value>) -> Value {
    Value::Object(m.clone())
}

/// A `{"tool_id", "tool", <key>}` row payload of the HITL and pause rows.
fn tool_payload(tool_id: &str, tool: Value, key: &str, value: Value) -> Option<Payload> {
    let mut m = Map::new();
    m.insert("tool_id".to_owned(), Value::String(tool_id.to_owned()));
    m.insert("tool".to_owned(), tool);
    m.insert(key.to_owned(), value);
    Some(Payload::Json(Value::Object(m)))
}

/// Go `map[string]any{"pause_kind": kind}`.
fn pause_extras(kind: &str) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("pause_kind".to_owned(), Value::String(kind.to_owned()));
    m
}

/// Go's `isNil` of a JSON value: absent or `null`.
fn is_nil_json(v: Option<&Value>) -> bool {
    v.is_none_or(Value::is_null)
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
        catch_unwind(AssertUnwindSafe(|| {
            let origin = resolve_tool_origin_for_call(t, Some(args));
            if origin != TOOL_ORIGIN_TRANSFER_AGENT {
                return origin;
            }
            let Some(target) = args
                .get("agent_name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
            else {
                return origin;
            };
            if let (true, true) = find_agent_a2a_by_name(t, target) {
                return TOOL_ORIGIN_TRANSFER_A2A;
            }
            if let Some(traces) = &self.traces {
                let mut target_id = id.clone();
                target_id.agent_name = target.to_owned();
                if let Some(target_agent) = traces.agent_named(ctx, &target_id)
                    && is_remote_a2a_agent(target_agent.as_ref())
                {
                    return TOOL_ORIGIN_TRANSFER_A2A;
                }
            }
            origin
        }))
        .unwrap_or(TOOL_ORIGIN_UNKNOWN)
    }

    /// Go `resumedToolOrigin(ctx, name)`: the origin of the tool a resumed function response
    /// answers, looked up in the runner agent's tree, then the highest known ancestor's.
    /// Toolset tools are not listed (listing them can do I/O), so they are UNKNOWN, as are
    /// names shared by tools of different origins.
    pub(crate) fn resumed_tool_origin(&self, ctx: &CallbackCtx, name: &str) -> &'static str {
        if name.is_empty() {
            return TOOL_ORIGIN_UNKNOWN;
        }
        let origin = catch_unwind(AssertUnwindSafe(|| {
            let runner = runner_agent_of(ctx);
            let (origin, found) = configured_tool_origin(runner.as_ref(), name);
            if found {
                return origin;
            }
            let Some(traces) = &self.traces else {
                return "";
            };
            // Look in the tree of the highest ancestor of runner seen so far.
            let root = traces.remember_agent_tree(runner.as_ref());
            let same = match (&root, &runner) {
                (Some(a), Some(b)) => AgentKey::of(a) == AgentKey::of(b),
                (None, None) => true,
                _ => false,
            };
            if same {
                return "";
            }
            configured_tool_origin(root.as_ref(), name).0
        }))
        .unwrap_or("");
        if origin.is_empty() {
            TOOL_ORIGIN_UNKNOWN
        } else {
            origin
        }
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
        let mut span_fn = || span.clone();
        for fr in msg
            .parts
            .iter()
            .filter_map(|p| p.function_response.as_ref())
        {
            if let Some(hitl) = hitl_tool(&fr.name) {
                let mut build = |d: &mut EventData| {
                    d.author = USER_AUTHOR.to_owned();
                    d.function_call_id = fr.id.clone();
                    d.content = tool_payload(
                        &fr.id,
                        Value::String(fr.name.clone()),
                        "result",
                        json_map(&fr.response),
                    );
                };
                self.log_session_span_event(
                    ctx,
                    id,
                    hitl.completed_event,
                    Some(&mut span_fn),
                    Some(&mut build),
                );
                continue;
            }
            let mut build = |d: &mut EventData| {
                d.author = USER_AUTHOR.to_owned();
                d.function_call_id = fr.id.clone();
                d.adk_extras = pause_extras("tool");
                let mut m = Map::new();
                m.insert("tool".to_owned(), Value::String(fr.name.clone()));
                m.insert("result".to_owned(), json_map(&fr.response));
                m.insert(
                    "tool_origin".to_owned(),
                    Value::String(self.resumed_tool_origin(ctx, &fr.name).to_owned()),
                );
                d.content = Some(Payload::Json(Value::Object(m)));
            };
            self.log_session_span_event(
                ctx,
                id,
                EVENT_TOOL_COMPLETED,
                Some(&mut span_fn),
                Some(&mut build),
            );
        }
    }

    /// Go `emitSessionEventSignals(ctx, ev)`: every signal row of a non-partial session event,
    /// without short-circuiting (through `log_session_event`; places from the trace manager's
    /// `event_span`). Partial events are ignored: their long-running IDs and state delta are
    /// repeated on the final aggregated event.
    pub(crate) fn emit_session_event_signals(&self, ctx: &CallbackCtx, ev: &Event) {
        if ev.response.partial {
            return;
        }
        let mut id = identity_of(ctx);
        if let Some(ni) = &ev.node_info
            && !ni.path.is_empty()
        {
            id.path = ni.path.clone();
        }
        // Divergence (ledger 7b-3a): Go dereferences its trace manager unconditionally (a nil manager panics into recoverCallback); gadk uses RowSpan::default() when the plugin has no trace manager.
        let span = self
            .traces
            .as_ref()
            .map(|tm| tm.event_span(ctx, &id, &ev.author, &ev.branch))
            .unwrap_or_default();
        let redact = self.config.redact_sensitive_keys();
        let max_len = self.config.max_content_length();
        let (clean_meta, a2a_trunc) =
            scrub_a2a_custom_metadata(&ev.response.custom_metadata, &self.custom_metadata, redact);
        let source = Arc::new(ev.clone());
        let apply_event_base = |d: &mut EventData| {
            d.source_event = Some(Arc::clone(&source));
            d.author = ev.author.clone();
            d.custom_metadata = clean_meta.clone();
            d.custom_metadata_scrubbed = true;
            d.custom_metadata_truncated = a2a_trunc;
        };

        // 1. STATE_DELTA (temp: keys scrubbed; the delta goes to attributes.state_delta only).
        if let Some(clean_delta) = ev
            .actions
            .state_delta
            .as_ref()
            .and_then(scrub_temp_state_delta)
        {
            let mut build = |d: &mut EventData| {
                apply_event_base(d);
                let mut attrs = Map::new();
                attrs.insert("state_delta".to_owned(), Value::Object(clean_delta.clone()));
                d.attributes = attrs;
            };
            self.log_session_event(ctx, &id, EVENT_STATE_DELTA, span.clone(), Some(&mut build));
        }

        // 2. AGENT_TRANSFER.
        let target = ev.actions.transfer_to_agent.trim();
        if !target.is_empty() {
            let mut build = |d: &mut EventData| {
                apply_event_base(d);
                let mut m = Map::new();
                m.insert("from_agent".to_owned(), Value::String(ev.author.clone()));
                m.insert("to_agent".to_owned(), Value::String(target.to_owned()));
                d.content = Some(Payload::Json(Value::Object(m)));
            };
            self.log_session_event(
                ctx,
                &id,
                EVENT_AGENT_TRANSFER,
                span.clone(),
                Some(&mut build),
            );
        }

        // 3. EVENT_COMPACTION.
        if self.should_log_session_event(EVENT_EVENT_COMPACTION)
            && let (Some(payload), comp_trunc, true) =
                extract_event_compaction(ev, clean_meta.as_ref(), max_len, redact)
        {
            let mut build = |d: &mut EventData| {
                apply_event_base(d);
                d.content = Some(Payload::Json(Value::Object(payload.clone())));
                d.content_truncated = comp_trunc || a2a_trunc;
            };
            self.log_session_event(
                ctx,
                &id,
                EVENT_EVENT_COMPACTION,
                span.clone(),
                Some(&mut build),
            );
        }

        // 4. AGENT_STATE_CHECKPOINT.
        if let Some((state, end_of_agent)) =
            clean_meta.as_ref().and_then(extract_agent_state_checkpoint)
        {
            let mut build = |d: &mut EventData| {
                apply_event_base(d);
                let mut m = Map::new();
                m.insert("agent_state".to_owned(), state.clone());
                m.insert("end_of_agent".to_owned(), Value::Bool(end_of_agent));
                d.content = Some(Payload::Json(Value::Object(m)));
                d.content_truncated = a2a_trunc;
            };
            self.log_session_event(
                ctx,
                &id,
                EVENT_AGENT_STATE_CHECKPOINT,
                span.clone(),
                Some(&mut build),
            );
        }

        // 5. HITL requests, TOOL_PAUSED and HITL completions.
        self.emit_hitl_and_pause_signals(ctx, &id, ev, &span, &apply_event_base);

        // 6. A2A_INTERACTION, NODE_ERROR, NODE_OUTPUT, AGENT_RESPONSE.
        if let Some(a2a_meta) = clean_meta.as_ref().and_then(extract_a2a_metadata) {
            let a2a_resp = a2a_meta.get("a2a:response");
            if !is_nil_json(a2a_meta.get("a2a:request")) || !is_nil_json(a2a_resp) {
                let mut build = |d: &mut EventData| {
                    apply_event_base(d);
                    d.content = a2a_resp
                        .filter(|v| !v.is_null())
                        .map(|v| Payload::Json(v.clone()));
                    d.content_truncated = a2a_trunc;
                    let mut attrs = Map::new();
                    attrs.insert("a2a_metadata".to_owned(), Value::Object(a2a_meta.clone()));
                    d.attributes = attrs;
                };
                self.log_session_event(
                    ctx,
                    &id,
                    EVENT_A2A_INTERACTION,
                    span.clone(),
                    Some(&mut build),
                );
            }
        }

        if let Some(ni) = ev.node_info.as_ref().filter(|ni| !ni.path.is_empty()) {
            let code = &ev.response.error_code;
            if !code.is_empty() && !is_model_response_code(code) {
                let mut build = |d: &mut EventData| {
                    apply_event_base(d);
                    d.failed = true;
                    d.err_msg = if ev.response.error_message.is_empty() {
                        code.clone()
                    } else {
                        ev.response.error_message.clone()
                    };
                    let mut m = Map::new();
                    m.insert("node_path".to_owned(), Value::String(ni.path.clone()));
                    m.insert("error_code".to_owned(), Value::String(code.clone()));
                    d.content = Some(Payload::Json(Value::Object(m)));
                };
                self.log_session_event(ctx, &id, EVENT_NODE_ERROR, span.clone(), Some(&mut build));
            }
            if let Some(output) = ev.output.as_ref().filter(|v| !v.is_null())
                && !ni.message_as_output
            {
                let mut build = |d: &mut EventData| {
                    apply_event_base(d);
                    d.content = Some(Payload::Json(output.clone()));
                };
                self.log_session_event(ctx, &id, EVENT_NODE_OUTPUT, span.clone(), Some(&mut build));
            }
        }

        if is_loggable_final_agent_response(ev)
            && let Some(content) = ev.response.content.as_ref()
        {
            let visible_parts = filter_visible_text_parts(&content.parts);
            if !visible_parts.is_empty() {
                let mut build = |d: &mut EventData| {
                    apply_event_base(d);
                    let visible = Content {
                        role: content.role.clone(),
                        parts: visible_parts.clone(),
                    };
                    let (summary, summary_trunc) =
                        format_response_content(Some(&visible), max_len, redact);
                    let (parts, parts_trunc) = if !self.denied(COLUMN_CONTENT_PARTS)
                        && self.config.log_multi_modal_content()
                        && self.config.content_formatter.is_none()
                    {
                        format_content_parts_with(Some(&visible), max_len, redact)
                    } else {
                        (Vec::new(), false)
                    };
                    let mut attrs = Map::new();
                    attrs.insert("event_author".to_owned(), Value::String(ev.author.clone()));
                    put_non_empty(&mut attrs, "source_event_id", &ev.id);
                    put_non_empty(&mut attrs, "source_event_author", &ev.author);
                    put_non_empty(&mut attrs, "source_event_branch", &ev.branch);
                    let mut m = Map::new();
                    m.insert("response".to_owned(), Value::String(summary));
                    d.content = Some(Payload::Json(Value::Object(m)));
                    d.parts = parts;
                    d.parts_truncated = parts_trunc;
                    d.content_truncated = summary_trunc;
                    d.attributes = attrs;
                };
                self.log_session_event(ctx, &id, EVENT_AGENT_RESPONSE, span, Some(&mut build));
            }
        }
    }

    /// Go `emitHITLAndPauseSignals(ctx, id, ev, span, applyEventBase)`: `TOOL_PAUSED` (and the
    /// HITL request) for each long-running call, HITL requests for the other HITL calls,
    /// `HITL_INPUT_REQUEST` for a workflow node's requested input, and HITL completions for
    /// HITL function responses.
    pub(crate) fn emit_hitl_and_pause_signals(
        &self,
        ctx: &CallbackCtx,
        id: &Identity,
        ev: &Event,
        span: &RowSpan,
        apply_event_base: &dyn Fn(&mut EventData),
    ) {
        let calls: Vec<&FunctionCall> = ev
            .response
            .content
            .iter()
            .flat_map(|c| c.parts.iter())
            .filter_map(|p| p.function_call.as_ref())
            .collect();
        let mut emitted_call_idx = vec![false; calls.len()];
        let mut emitted_input = false;

        for tool_id in &ev.long_running_tool_ids {
            let mut match_idx = calls.iter().position(|fc| &fc.id == tool_id);
            if match_idx.is_none()
                && calls.len() == 1
                && !emitted_call_idx[0]
                && calls[0].id.is_empty()
            {
                match_idx = Some(0);
            }

            if let Some(idx) = match_idx {
                emitted_call_idx[idx] = true;
                let fc = calls[idx];
                let resolved_id = if tool_id.is_empty() {
                    fc.id.as_str()
                } else {
                    tool_id.as_str()
                };
                let mut pause_kind = "tool";
                let args = json_map(&fc.args);
                if let Some(hitl) = hitl_tool(&fc.name) {
                    pause_kind = hitl.pause_kind;
                    if fc.name == WORKFLOW_INPUT_FUNCTION_CALL_NAME {
                        emitted_input = true;
                    }
                    let mut build = |d: &mut EventData| {
                        apply_event_base(d);
                        d.function_call_id = resolved_id.to_owned();
                        d.content_truncated = false;
                        d.content = tool_payload(
                            resolved_id,
                            Value::String(fc.name.clone()),
                            "args",
                            args.clone(),
                        );
                    };
                    self.log_session_event(
                        ctx,
                        id,
                        hitl.request_event,
                        span.clone(),
                        Some(&mut build),
                    );
                }

                let mut build = |d: &mut EventData| {
                    apply_event_base(d);
                    d.function_call_id = resolved_id.to_owned();
                    d.content_truncated = false;
                    d.adk_extras = pause_extras(pause_kind);
                    d.content = tool_payload(
                        resolved_id,
                        Value::String(fc.name.clone()),
                        "args",
                        args.clone(),
                    );
                };
                self.log_session_event(ctx, id, EVENT_TOOL_PAUSED, span.clone(), Some(&mut build));
                continue;
            }

            // A long-running tool ID with no matching function call part.
            let mut build = |d: &mut EventData| {
                apply_event_base(d);
                d.function_call_id = tool_id.clone();
                d.adk_extras = pause_extras("tool");
                d.content = tool_payload(tool_id, Value::Null, "args", Value::Null);
            };
            self.log_session_event(ctx, id, EVENT_TOOL_PAUSED, span.clone(), Some(&mut build));
        }

        // HITL requests for the HITL calls not matched through the long-running IDs.
        for (idx, fc) in calls.iter().enumerate() {
            if emitted_call_idx[idx] {
                continue;
            }
            let Some(hitl) = hitl_tool(&fc.name) else {
                continue;
            };
            if fc.name == WORKFLOW_INPUT_FUNCTION_CALL_NAME {
                emitted_input = true;
            }
            let mut build = |d: &mut EventData| {
                apply_event_base(d);
                d.function_call_id = fc.id.clone();
                d.content_truncated = false;
                d.content = tool_payload(
                    &fc.id,
                    Value::String(fc.name.clone()),
                    "args",
                    json_map(&fc.args),
                );
            };
            self.log_session_event(ctx, id, hitl.request_event, span.clone(), Some(&mut build));
        }

        // HITL_INPUT_REQUEST from the requested input, unless a call already emitted one.
        if let Some(ri) = ev.requested_input.as_ref()
            && !emitted_input
        {
            let requested = serde_json::to_value(ri).unwrap_or(Value::Null);
            let mut build = |d: &mut EventData| {
                apply_event_base(d);
                let mut m = Map::new();
                m.insert(
                    "tool".to_owned(),
                    Value::String(WORKFLOW_INPUT_FUNCTION_CALL_NAME.to_owned()),
                );
                m.insert("args".to_owned(), requested.clone());
                m.insert("requested_input".to_owned(), requested.clone());
                d.content = Some(Payload::Json(Value::Object(m)));
            };
            self.log_session_event(
                ctx,
                id,
                EVENT_HITL_INPUT_REQUEST,
                span.clone(),
                Some(&mut build),
            );
        }

        // HITL_*_COMPLETED for the HITL function responses.
        let responses = ev
            .response
            .content
            .iter()
            .flat_map(|c| c.parts.iter())
            .filter_map(|p| p.function_response.as_ref());
        for fr in responses {
            let Some(hitl) = hitl_tool(&fr.name) else {
                continue;
            };
            let mut build = |d: &mut EventData| {
                apply_event_base(d);
                d.function_call_id = fr.id.clone();
                d.content_truncated = false;
                d.content = tool_payload(
                    &fr.id,
                    Value::String(fr.name.clone()),
                    "result",
                    json_map(&fr.response),
                );
            };
            self.log_session_event(
                ctx,
                id,
                hitl.completed_event,
                span.clone(),
                Some(&mut build),
            );
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::any::Any;

    use chrono::TimeZone;
    use gadk_agent::{AgentConfig, AgentState};
    use gadk_engine::llm::LlmAgentConfig;
    use gadk_genai::FunctionResponse;
    use gadk_session::RequestInput;
    use serde_json::json;

    use super::*;

    /// A tool of a given kind, optionally wrapping another or running an agent.
    struct FakeTool {
        name: String,
        kind: ToolOriginKind,
        inner: Option<Arc<dyn Tool>>,
        agent: Option<Arc<dyn Agent>>,
        panics: bool,
    }

    impl Tool for FakeTool {
        fn name(&self) -> &str {
            &self.name
        }
        fn description(&self) -> &str {
            ""
        }
        fn unwrap(&self) -> Option<Arc<dyn Tool>> {
            self.inner.clone()
        }
        fn origin_kind(&self) -> ToolOriginKind {
            if self.panics {
                panic!("origin_kind panics");
            }
            self.kind
        }
        fn origin_agent(&self) -> Option<Arc<dyn Agent>> {
            self.agent.clone()
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    fn tool(name: &str, kind: ToolOriginKind) -> Arc<dyn Tool> {
        Arc::new(FakeTool {
            name: name.to_owned(),
            kind,
            inner: None,
            agent: None,
            panics: false,
        })
    }

    fn agent_tool(name: &str, kind: ToolOriginKind, agent: Arc<dyn Agent>) -> Arc<dyn Tool> {
        Arc::new(FakeTool {
            name: name.to_owned(),
            kind,
            inner: None,
            agent: Some(agent),
            panics: false,
        })
    }

    /// `t` under `n` wrappers (each `Unknown`, unwrapping to the next).
    fn wrapped(t: Arc<dyn Tool>, n: usize) -> Arc<dyn Tool> {
        (0..n).fold(t, |inner, i| {
            Arc::new(FakeTool {
                name: format!("wrapper{i}"),
                kind: ToolOriginKind::Unknown,
                inner: Some(inner),
                agent: None,
                panics: false,
            }) as Arc<dyn Tool>
        })
    }

    fn agent_of(name: &str, agent_type: AgentType, subs: Vec<Arc<dyn Agent>>) -> Arc<dyn Agent> {
        gadk_agent::new_with_state(
            AgentConfig {
                name: name.to_owned(),
                sub_agents: subs,
                ..AgentConfig::default()
            },
            AgentState {
                agent_type,
                config: None,
                on_before_agent_callback_event: None,
                live: None,
            },
        )
        .unwrap()
    }

    fn remote(name: &str) -> Arc<dyn Agent> {
        agent_of(name, AgentType::RemoteAgent, Vec::new())
    }

    fn local(name: &str, subs: Vec<Arc<dyn Agent>>) -> Arc<dyn Agent> {
        agent_of(name, AgentType::CustomAgent, subs)
    }

    fn llm(name: &str, tools: Vec<Arc<dyn Tool>>, subs: Vec<Arc<dyn Agent>>) -> Arc<dyn Agent> {
        gadk_engine::llm::agent::new(LlmAgentConfig {
            name: name.to_owned(),
            tools,
            sub_agents: subs,
            ..LlmAgentConfig::default()
        })
        .unwrap()
    }

    fn obj(v: Value) -> Map<String, Value> {
        match v {
            Value::Object(m) => m,
            other => panic!("not an object: {other}"),
        }
    }

    fn text_content(role: &str, text: &str) -> Content {
        Content {
            role: role.to_owned(),
            parts: vec![Part {
                text: text.to_owned(),
                ..Part::default()
            }],
        }
    }

    #[test]
    fn hitl_tool_maps_the_three_hitl_tools() {
        let cred = hitl_tool("adk_request_credential").unwrap();
        assert_eq!(cred.request_event, "HITL_CREDENTIAL_REQUEST");
        assert_eq!(cred.completed_event, "HITL_CREDENTIAL_REQUEST_COMPLETED");
        assert_eq!(cred.pause_kind, "hitl_credential");
        let conf = hitl_tool("adk_request_confirmation").unwrap();
        assert_eq!(conf.request_event, "HITL_CONFIRMATION_REQUEST");
        assert_eq!(conf.completed_event, "HITL_CONFIRMATION_REQUEST_COMPLETED");
        assert_eq!(conf.pause_kind, "hitl_confirmation");
        let input = hitl_tool("adk_request_input").unwrap();
        assert_eq!(input.request_event, "HITL_INPUT_REQUEST");
        assert_eq!(input.completed_event, "HITL_INPUT_REQUEST_COMPLETED");
        assert_eq!(input.pause_kind, "hitl_input");
        assert_eq!(hitl_tool("lookup"), None);
        assert_eq!(
            hitl_tool(" adk_request_input"),
            None,
            "names are matched exactly"
        );
    }

    #[test]
    fn model_response_codes_are_go_genai_reasons() {
        for code in [
            "STOP",
            " MAX_TOKENS ",
            "FINISH_REASON_UNSPECIFIED",
            "IMAGE_OTHER",
            "BLOCKED_REASON_UNSPECIFIED",
            "MODEL_ARMOR",
            "JAILBREAK",
        ] {
            assert!(is_model_response_code(code), "{code}");
        }
        // Not in Go's genai version: gadk-genai's newer finish reasons and node error codes.
        for code in [
            "TOO_MANY_TOOL_CALLS",
            "CONTINUATION",
            "NODE_FAILED",
            "",
            "stop",
        ] {
            assert!(!is_model_response_code(code), "{code}");
        }
    }

    #[test]
    fn tool_origin_of_classifies_every_origin_kind() {
        let cases = [
            (ToolOriginKind::Unknown, "UNKNOWN"),
            (ToolOriginKind::Local, "LOCAL"),
            (ToolOriginKind::Mcp, "MCP"),
            (ToolOriginKind::TransferAgent, "TRANSFER_AGENT"),
            (ToolOriginKind::SubAgent, "SUB_AGENT"),
            (ToolOriginKind::Skill, "SKILL"),
            (ToolOriginKind::Toolbox, "TOOLBOX"),
        ];
        for (kind, want) in cases {
            assert_eq!(tool_origin_of(&tool("t", kind)), want, "{kind:?}");
        }
    }

    #[test]
    fn sub_agent_tools_are_a2a_when_their_agent_is_remote() {
        let sub = agent_tool("t", ToolOriginKind::SubAgent, local("helper", Vec::new()));
        assert_eq!(tool_origin_of(&sub), TOOL_ORIGIN_SUB_AGENT);
        let a2a = agent_tool("t", ToolOriginKind::SubAgent, remote("remote"));
        assert_eq!(tool_origin_of(&a2a), TOOL_ORIGIN_A2A);
        // A remote agent on a tool of another kind does not change its origin.
        let local_tool = agent_tool("t", ToolOriginKind::Local, remote("remote"));
        assert_eq!(tool_origin_of(&local_tool), TOOL_ORIGIN_LOCAL);
        assert_eq!(sub_agent_or_a2a(None), TOOL_ORIGIN_SUB_AGENT);
        assert!(is_remote_a2a_agent(remote("r").as_ref()));
        assert!(!is_remote_a2a_agent(local("l", Vec::new()).as_ref()));
    }

    #[test]
    fn wrappers_are_unwrapped_within_the_hop_budget() {
        let inner = tool("inner", ToolOriginKind::Mcp);
        let ok = wrapped(Arc::clone(&inner), MAX_UNWRAP_HOPS - 1);
        assert!(Arc::ptr_eq(&unwrap_tool(&ok).unwrap(), &inner));
        assert_eq!(tool_origin_of(&ok), TOOL_ORIGIN_MCP);
        let too_deep = wrapped(inner, MAX_UNWRAP_HOPS);
        assert!(unwrap_tool(&too_deep).is_none());
        assert_eq!(tool_origin_of(&too_deep), TOOL_ORIGIN_UNKNOWN);
        // A wrapper's own kind is ignored: the unwrapped tool decides.
        let a2a = wrapped(
            agent_tool("t", ToolOriginKind::SubAgent, remote("remote")),
            1,
        );
        assert_eq!(tool_origin_of(&a2a), TOOL_ORIGIN_A2A);
    }

    #[test]
    fn resolve_tool_origin_for_call_checks_the_name_after_the_kind() {
        let unknown_transfer = tool("transfer_to_agent", ToolOriginKind::Unknown);
        assert_eq!(
            resolve_tool_origin_for_call(&unknown_transfer, None),
            TOOL_ORIGIN_TRANSFER_AGENT
        );
        let spaced = tool(" transfer_to_agent ", ToolOriginKind::Unknown);
        assert_eq!(
            resolve_tool_origin_for_call(&spaced, None),
            TOOL_ORIGIN_TRANSFER_AGENT
        );
        let local_transfer = tool("transfer_to_agent", ToolOriginKind::Local);
        assert_eq!(
            resolve_tool_origin_for_call(&local_transfer, None),
            TOOL_ORIGIN_LOCAL
        );
        let unknown = tool("lookup", ToolOriginKind::Unknown);
        assert_eq!(
            resolve_tool_origin_for_call(&unknown, None),
            TOOL_ORIGIN_UNKNOWN
        );
        let panicking: Arc<dyn Tool> = Arc::new(FakeTool {
            name: "transfer_to_agent".to_owned(),
            kind: ToolOriginKind::Local,
            inner: None,
            agent: None,
            panics: true,
        });
        assert_eq!(
            resolve_tool_origin_for_call(&panicking, None),
            TOOL_ORIGIN_UNKNOWN
        );
    }

    #[test]
    fn find_agent_a2a_by_name_searches_the_tool_agent_tree() {
        let tree = local(
            "coordinator",
            vec![remote("remote"), local("helper", Vec::new())],
        );
        let t = agent_tool("transfer_to_agent", ToolOriginKind::TransferAgent, tree);
        assert_eq!(find_agent_a2a_by_name(&t, "coordinator"), (true, false));
        assert_eq!(find_agent_a2a_by_name(&t, "remote"), (true, true));
        assert_eq!(find_agent_a2a_by_name(&t, "helper"), (true, false));
        assert_eq!(find_agent_a2a_by_name(&t, "missing"), (false, false));
        let no_agent = tool("transfer_to_agent", ToolOriginKind::TransferAgent);
        assert_eq!(find_agent_a2a_by_name(&no_agent, "remote"), (false, false));
        assert_eq!(
            find_agent_a2a_by_name(&wrapped(no_agent, MAX_UNWRAP_HOPS), "remote"),
            (false, false)
        );
    }

    /// Go's `supportedAgents` slice: each agent in order, by name, then through its tree.
    #[test]
    fn find_agent_a2a_by_name_searches_every_tool_agent() {
        struct Targets(Vec<Arc<dyn Agent>>);
        impl Tool for Targets {
            fn name(&self) -> &str {
                "transfer_to_agent"
            }
            fn description(&self) -> &str {
                ""
            }
            fn origin_kind(&self) -> ToolOriginKind {
                ToolOriginKind::TransferAgent
            }
            fn origin_agents(&self) -> Vec<Arc<dyn Agent>> {
                self.0.clone()
            }
            fn as_any(&self) -> &dyn Any {
                self
            }
        }
        let t: Arc<dyn Tool> = Arc::new(Targets(vec![
            local("helper", Vec::new()),
            remote("remote"),
            local("parent", vec![remote("deep")]),
        ]));
        assert_eq!(find_agent_a2a_by_name(&t, "helper"), (true, false));
        assert_eq!(find_agent_a2a_by_name(&t, "remote"), (true, true));
        assert_eq!(find_agent_a2a_by_name(&t, "deep"), (true, true));
        assert_eq!(find_agent_a2a_by_name(&t, "missing"), (false, false));
    }

    #[test]
    fn resolve_tool_origin_tells_transfers_to_remote_agents() {
        let p = AnalyticsPlugin::default();
        let ctx = CallbackCtx::background();
        let id = Identity::default();
        let tree = local(
            "coordinator",
            vec![remote("remote"), local("helper", Vec::new())],
        );
        let t = agent_tool("transfer_to_agent", ToolOriginKind::TransferAgent, tree);
        let args = |name: &str| obj(json!({ "agent_name": name }));
        assert_eq!(
            p.resolve_tool_origin(&ctx, &id, &t, &args(" remote ")),
            TOOL_ORIGIN_TRANSFER_A2A
        );
        assert_eq!(
            p.resolve_tool_origin(&ctx, &id, &t, &args("helper")),
            TOOL_ORIGIN_TRANSFER_AGENT
        );
        assert_eq!(
            p.resolve_tool_origin(&ctx, &id, &t, &args("  ")),
            TOOL_ORIGIN_TRANSFER_AGENT
        );
        assert_eq!(
            p.resolve_tool_origin(&ctx, &id, &t, &obj(json!({ "agent_name": 7 }))),
            TOOL_ORIGIN_TRANSFER_AGENT
        );
        // Only transfers look at the target.
        let sub = agent_tool("helper_tool", ToolOriginKind::SubAgent, remote("remote"));
        assert_eq!(
            p.resolve_tool_origin(&ctx, &id, &sub, &args("remote")),
            TOOL_ORIGIN_A2A
        );
        let named = tool("transfer_to_agent", ToolOriginKind::Unknown);
        assert_eq!(
            p.resolve_tool_origin(&ctx, &id, &named, &args("remote")),
            TOOL_ORIGIN_TRANSFER_AGENT,
            "no agent tree and no trace manager: the target is not found"
        );
    }

    #[test]
    fn configured_tool_origin_walks_the_llm_agents_of_the_tree() {
        let leaf = llm(
            "leaf",
            vec![
                tool("search", ToolOriginKind::Mcp),
                tool("shared", ToolOriginKind::Mcp),
            ],
            Vec::new(),
        );
        let custom = local("custom", vec![leaf]);
        let root = llm(
            "root",
            vec![
                tool("lookup", ToolOriginKind::Local),
                tool("shared", ToolOriginKind::Local),
                tool("same", ToolOriginKind::Skill),
            ],
            vec![
                custom,
                llm(
                    "other",
                    vec![tool("same", ToolOriginKind::Skill)],
                    Vec::new(),
                ),
            ],
        );
        assert_eq!(
            configured_tool_origin(Some(&root), "lookup"),
            ("LOCAL", true)
        );
        assert_eq!(configured_tool_origin(Some(&root), "search"), ("MCP", true));
        assert_eq!(configured_tool_origin(Some(&root), "same"), ("SKILL", true));
        assert_eq!(
            configured_tool_origin(Some(&root), "shared"),
            ("UNKNOWN", true),
            "tools of different origins"
        );
        assert_eq!(configured_tool_origin(Some(&root), "missing"), ("", false));
        assert_eq!(configured_tool_origin(None, "lookup"), ("", false));
    }

    #[test]
    fn resumed_tool_origin_of_an_unnamed_response_is_unknown() {
        let p = AnalyticsPlugin::default();
        assert_eq!(
            p.resumed_tool_origin(&CallbackCtx::background(), ""),
            TOOL_ORIGIN_UNKNOWN
        );
    }

    #[test]
    fn scrub_temp_state_delta_drops_temp_keys() {
        let delta =
            obj(json!({ "temp:scratch": 1, " temp:spaced": 2, "TEMP:upper": 3, "kept": 4 }));
        assert_eq!(
            scrub_temp_state_delta(&delta),
            Some(obj(json!({ "TEMP:upper": 3, "kept": 4 })))
        );
        assert_eq!(
            scrub_temp_state_delta(&obj(json!({ "temp:only": 1 }))),
            None
        );
        assert_eq!(scrub_temp_state_delta(&Map::new()), None);
    }

    #[test]
    fn extract_a2a_metadata_keeps_a2a_keys_case_insensitively() {
        let meta = obj(json!({
            "a2a:request": { "q": 1 },
            " A2A:task_id": "t1",
            "other": true,
        }));
        assert_eq!(
            extract_a2a_metadata(&meta),
            Some(obj(
                json!({ "a2a:request": { "q": 1 }, " A2A:task_id": "t1" })
            ))
        );
        assert_eq!(extract_a2a_metadata(&obj(json!({ "other": 1 }))), None);
        assert_eq!(extract_a2a_metadata(&Map::new()), None);
    }

    #[test]
    fn extract_agent_state_checkpoint_matches_go_cases() {
        let cases = [
            (
                json!({ "adk_agent_state": { "step": 2, "cursor": "abc" }, "end_of_agent": true }),
                Some((json!({ "step": 2, "cursor": "abc" }), true)),
            ),
            (
                json!({ "agent_state": { "step": 3 }, "end_of_agent": false }),
                Some((json!({ "step": 3 }), false)),
            ),
            (
                json!({ "agent_state": null, "end_of_agent": true }),
                Some((Value::Null, true)),
            ),
            (
                json!({ "adk_end_of_agent": true }),
                Some((Value::Null, true)),
            ),
            (json!({ "agent_state": null, "end_of_agent": false }), None),
            (
                json!({ "adk_agent_state": null, "agent_state": { "a": 1 } }),
                Some((json!({ "a": 1 }), false)),
            ),
            (
                json!({ "adk_agent_state": { "x": 1 }, "agent_state": { "a": 1 } }),
                Some((json!({ "x": 1 }), false)),
            ),
            (json!({ "end_of_agent": "true" }), None),
            (json!({}), None),
        ];
        for (meta, want) in cases {
            assert_eq!(
                extract_agent_state_checkpoint(&obj(meta.clone())),
                want,
                "{meta}"
            );
        }
    }

    #[test]
    fn epoch_seconds_is_fractional_and_null_for_the_zero_time() {
        let ts = Utc.timestamp_opt(1_700_000_000, 500_000_000).unwrap();
        assert_eq!(epoch_seconds(Some(ts)), json!(1_700_000_000.5));
        assert_eq!(epoch_seconds(None), Value::Null);
        assert_eq!(epoch_seconds(Some(zero_time())), Value::Null);
    }

    fn compaction_event(meta: Value) -> Event {
        let mut ev = Event::default();
        ev.response.custom_metadata = obj(meta);
        ev
    }

    #[test]
    fn extract_event_compaction_reads_actions_compaction() {
        let mut ev = Event::default();
        ev.actions.compaction = Some(EventCompaction {
            start_timestamp: Utc.timestamp_opt(1_700_000_000, 500_000_000).unwrap(),
            end_timestamp: Utc.timestamp_opt(1_700_000_300, 250_000_000).unwrap(),
            compacted_content: Some(text_content("model", "short summary")),
            excluded_events: Vec::new(),
        });
        assert!(has_actions_compaction(&ev.actions));
        let (payload, trunc, ok) = extract_event_compaction(&ev, None, 100, true);
        assert!(ok && !trunc);
        assert_eq!(
            Value::Object(payload.unwrap()),
            json!({
                "start_timestamp": 1_700_000_000.5,
                "end_timestamp": 1_700_000_300.25,
                "compacted_content": "text: 'short summary'",
            })
        );
        // The typed compaction wins over the metadata.
        let meta = obj(json!({ "adk_compaction": { "compacted_content": "meta" } }));
        let (payload, _, _) = extract_event_compaction(&ev, Some(&meta), 100, true);
        assert_eq!(
            payload.unwrap()["compacted_content"],
            json!("text: 'short summary'")
        );
    }

    #[test]
    fn extract_event_compaction_truncates_the_summary() {
        let mut ev = Event::default();
        ev.actions.compaction = Some(EventCompaction {
            start_timestamp: Utc.timestamp_opt(1_700_000_000, 500_000_000).unwrap(),
            end_timestamp: Utc.timestamp_opt(1_700_000_300, 250_000_000).unwrap(),
            compacted_content: Some(text_content(
                "model",
                "this compacted summary is longer than 40 chars",
            )),
            excluded_events: Vec::new(),
        });
        let (payload, trunc, ok) = extract_event_compaction(&ev, None, 40, true);
        assert!(ok && trunc);
        assert_eq!(
            payload.unwrap()["compacted_content"],
            json!("text: 'this compacted summary is longer ...[TRUNCATED]")
        );
    }

    #[test]
    fn extract_event_compaction_reads_adk_compaction_metadata() {
        // Sibling keys are dropped; numbers are epoch seconds already.
        let ev = compaction_event(json!({}));
        let meta = obj(json!({ "adk_compaction": {
            "start_timestamp": 1_700_000_000.0,
            "end_timestamp": 1_700_000_100.0,
            "compacted_content": "conversation summary text",
            "ssn": "123-45-6789",
        }}));
        let (payload, trunc, ok) = extract_event_compaction(&ev, Some(&meta), -1, true);
        assert!(ok && !trunc);
        assert_eq!(
            Value::Object(payload.unwrap()),
            json!({
                "start_timestamp": 1_700_000_000.0,
                "end_timestamp": 1_700_000_100.0,
                "compacted_content": "conversation summary text",
            })
        );
        // Field-name and JSON-name aliases; RFC 3339 strings; content maps; zero values.
        let meta = obj(json!({ "adk_compaction": {
            "StartTimestamp": "2023-11-14T22:13:20.5Z",
            "endTimestamp": 0,
            "compactedContent": { "role": "model", "parts": [{ "text": "from a map" }] },
        }}));
        let (payload, _, ok) = extract_event_compaction(&ev, Some(&meta), -1, true);
        assert!(ok);
        assert_eq!(
            Value::Object(payload.unwrap()),
            json!({
                "start_timestamp": 1_700_000_000.5,
                "end_timestamp": null,
                "compacted_content": "text: 'from a map'",
            })
        );
        // An existing payload key wins over its alias.
        let meta = obj(json!({ "adk_compaction": {
            "compacted_content": "kept",
            "CompactedContent": "dropped",
        }}));
        let (payload, _, _) = extract_event_compaction(&ev, Some(&meta), -1, true);
        assert_eq!(payload.unwrap()["compacted_content"], json!("kept"));
        // Unrelated maps, non-maps and unparseable values.
        for v in [
            json!({ "unrelated": "ignored" }),
            json!("text"),
            json!(null),
        ] {
            let meta = obj(json!({ "adk_compaction": v }));
            assert_eq!(
                extract_event_compaction(&ev, Some(&meta), -1, true),
                (None, false, false)
            );
        }
        let meta = obj(json!({ "adk_compaction": {
            "start_timestamp": "yesterday",
            "compacted_content": { "parts": [] },
        }}));
        let (payload, _, ok) = extract_event_compaction(&ev, Some(&meta), -1, true);
        assert!(ok, "a known key is enough");
        assert_eq!(
            Value::Object(payload.unwrap()),
            json!({ "start_timestamp": null, "end_timestamp": null, "compacted_content": "" })
        );
        // Other metadata keys are not compaction.
        let meta = obj(json!({ "compaction": { "compacted_content": "app" } }));
        assert_eq!(
            extract_event_compaction(&ev, Some(&meta), -1, true),
            (None, false, false)
        );
        assert_eq!(
            extract_event_compaction(&ev, None, -1, true),
            (None, false, false)
        );
    }

    #[test]
    fn compaction_summaries_are_redacted() {
        let ev = Event::default();
        let meta = obj(json!({ "adk_compaction": {
            "compacted_content": "login auth_code=MY_AUTH_CODE and code_verifier=MY_VERIFIER",
        }}));
        let (payload, _, ok) = extract_event_compaction(&ev, Some(&meta), -1, true);
        assert!(ok);
        let text = payload.unwrap()["compacted_content"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(
            !text.contains("MY_AUTH_CODE") && !text.contains("MY_VERIFIER"),
            "{text}"
        );
        assert!(text.contains("[REDACTED]"), "{text}");
        let (payload, _, _) = extract_event_compaction(&ev, Some(&meta), -1, false);
        assert!(
            payload.unwrap()["compacted_content"]
                .as_str()
                .unwrap()
                .contains("MY_AUTH_CODE")
        );
    }

    fn call_part(name: &str) -> Part {
        Part {
            function_call: Some(FunctionCall {
                name: name.to_owned(),
                ..FunctionCall::default()
            }),
            ..Part::default()
        }
    }

    fn response_part(name: &str) -> Part {
        Part {
            function_response: Some(FunctionResponse {
                name: name.to_owned(),
                ..FunctionResponse::default()
            }),
            ..Part::default()
        }
    }

    fn event_with(parts: Vec<Part>) -> Event {
        let mut ev = Event {
            author: "agent".to_owned(),
            ..Event::default()
        };
        ev.response.content = Some(Content {
            role: "model".to_owned(),
            parts,
        });
        ev
    }

    #[test]
    fn final_response_predicates() {
        let text = Part {
            text: "answer".to_owned(),
            ..Part::default()
        };
        assert!(!has_function_call(None));
        assert!(has_function_call(
            event_with(vec![call_part("f")]).response.content.as_ref()
        ));
        assert!(!has_function_call(
            event_with(vec![text.clone()]).response.content.as_ref()
        ));

        let plain = event_with(vec![text.clone()]);
        assert!(is_final_response(&plain));
        assert!(is_loggable_final_agent_response(&plain));

        assert!(!is_final_response(&Event::default()), "no content");
        let mut no_content = plain.clone();
        no_content.response.content = None;
        no_content.actions.skip_summarization = true;
        assert!(!is_loggable_final_agent_response(&no_content));

        let mut partial = plain.clone();
        partial.response.partial = true;
        assert!(!is_loggable_final_agent_response(&partial));

        let mut user = plain.clone();
        user.author = USER_AUTHOR.to_owned();
        assert!(!is_loggable_final_agent_response(&user));

        assert!(!is_loggable_final_agent_response(&event_with(Vec::new())));

        let mut long_running = plain.clone();
        long_running.long_running_tool_ids = vec!["call-1".to_owned()];
        assert!(!is_loggable_final_agent_response(&long_running));

        let call = event_with(vec![text.clone(), call_part("f")]);
        assert!(!is_loggable_final_agent_response(&call));

        let mut compaction = plain.clone();
        compaction.actions.compaction = Some(EventCompaction {
            start_timestamp: zero_time(),
            end_timestamp: zero_time(),
            compacted_content: None,
            excluded_events: Vec::new(),
        });
        assert!(!is_loggable_final_agent_response(&compaction));

        // A function response with answer text is final only with skip-summarization (Go's
        // agenttool display event).
        let mut display = event_with(vec![response_part("agent_tool"), text]);
        assert!(!is_final_response(&display));
        assert!(!is_loggable_final_agent_response(&display));
        display.actions.skip_summarization = true;
        assert!(is_loggable_final_agent_response(&display));
    }

    #[test]
    fn filter_visible_text_parts_drops_thoughts_and_non_text() {
        let visible = Part {
            text: "shown".to_owned(),
            ..Part::default()
        };
        let thought = Part {
            text: "hidden".to_owned(),
            thought: true,
            ..Part::default()
        };
        let parts = vec![thought, call_part("f"), visible.clone(), Part::default()];
        assert_eq!(filter_visible_text_parts(&parts), vec![visible]);
        assert!(filter_visible_text_parts(&[]).is_empty());
    }

    #[test]
    fn scrub_a2a_custom_metadata_keeps_allowed_and_a2a_keys() {
        let allow = CustomMetadataFilter::new(&["tenant".to_owned(), "ckpt_*".to_owned()]);
        let meta = obj(json!({
            "tenant": "acme",
            "ckpt_step": 2,
            "secret_stuff": "dropped",
            "A2A:upper": "dropped (prefix is case-sensitive)",
            "a2a:api_key": "k-123",
            "a2a:request": { "note": "exchange auth_code=MY_A2A_CODE and code_verifier=MY_A2A_VERIFIER", "password": "pw" },
        }));
        let (clean, trunc) = scrub_a2a_custom_metadata(&meta, &allow, true);
        assert!(!trunc);
        let clean = clean.unwrap();
        assert_eq!(
            clean.keys().cloned().collect::<Vec<_>>(),
            ["a2a:api_key", "a2a:request", "ckpt_step", "tenant"]
        );
        assert_eq!(clean["a2a:api_key"], json!("[REDACTED]"));
        // Key-based redaction only: free text is scrubbed later, by the row's JSON column
        // (Go `jsonColumn`), as in Go.
        assert_eq!(
            clean["a2a:request"],
            json!({
                "note": "exchange auth_code=MY_A2A_CODE and code_verifier=MY_A2A_VERIFIER",
                "password": "[REDACTED]",
            })
        );
        assert_eq!(clean["tenant"], json!("acme"));

        let (raw, _) = scrub_a2a_custom_metadata(&meta, &allow, false);
        assert_eq!(raw.unwrap()["a2a:api_key"], json!("k-123"));

        let none = CustomMetadataFilter::default();
        assert_eq!(
            scrub_a2a_custom_metadata(&obj(json!({ "tenant": "acme" })), &none, true),
            (None, false)
        );
        assert_eq!(
            scrub_a2a_custom_metadata(&Map::new(), &allow, true),
            (None, false)
        );
    }

    #[test]
    fn helpers_build_go_payload_shapes() {
        let Some(Payload::Json(v)) = tool_payload("id-1", Value::Null, "args", Value::Null) else {
            panic!("tool_payload is a JSON payload");
        };
        assert_eq!(v, json!({ "tool_id": "id-1", "tool": null, "args": null }));
        assert_eq!(pause_extras("tool"), obj(json!({ "pause_kind": "tool" })));
        assert!(is_nil_json(None));
        assert!(is_nil_json(Some(&Value::Null)));
        assert!(!is_nil_json(Some(&json!({}))));
        // The requested-input payload is the input's JSON form (Go marshals the struct).
        let ri = RequestInput {
            interrupt_id: "int-1".to_owned(),
            ..RequestInput::default()
        };
        assert_eq!(
            serde_json::to_value(&ri).unwrap()["interruptId"],
            json!("int-1")
        );
    }
}
