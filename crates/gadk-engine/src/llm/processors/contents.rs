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

//! Contents request processor and the display-name scrubber.

use std::collections::{BTreeSet, HashMap, HashSet};

use futures::future::BoxFuture;
use gadk_agent::{AgentError, InvocationContext, Mode, REQUEST_CONFIRMATION_FUNCTION_CALL_NAME};
use gadk_genai::{Content, Part};
use gadk_model::{LlmRequest, LlmResponse, is_internal_live_artifact_part};
use gadk_session::Event;
use serde_json::{Map, Value};

use crate::{
    compaction::{
        apply::{apply, has_usable_summary},
        runtime,
    },
    llm::{
        config::IncludeContents,
        flow::Flow,
        mode::{bound_mode, mode_for},
        processors::{go_json_marshal, go_quote_list, wrap_err},
        state::reveal,
        transfer::TRANSFER_TO_AGENT_TOOL_NAME,
    },
    tool::utils::{
        content, event_belongs_to_branch, function_calls, function_responses,
        remove_client_function_call_id,
    },
};

/// Appended as a second user-content text part for single_turn agents.
pub(crate) const SINGLE_TURN_NUDGE: &str = "Important: You will not receive any user replies or clarifications. Complete the task using only the information provided above.";

const CONTINUE_PROCESSING: &str = "Continue processing previous requests as instructed. Exit or provide a summary if no more outputs are needed.";

/// The special function that handles credential requests.
const REQUEST_EUC_FUNCTION_CALL_NAME: &str = "adk_request_credential";

/// Populates the request contents
/// from the session's events.
pub(crate) fn contents_request_processor<'a>(
    ctx: &'a InvocationContext,
    req: &'a mut LlmRequest,
    _f: &'a Flow,
) -> BoxFuture<'a, Result<Vec<Event>, AgentError>> {
    Box::pin(async move {
        let agent = ctx.agent();
        let Some(state) = reveal(&*agent) else {
            return Ok(Vec::new());
        };
        let name = agent.name().to_owned();
        // History hiding follows the PLACEMENT (bound mode), not the declaration; an explicit
        // IncludeContents::Default opts out of it; Unset behaves as Default everywhere else
        let placement_hides_history = bound_mode(ctx, &*agent) == Some(Mode::SingleTurn)
            && state.cfg.include_contents != IncludeContents::Default;
        let current_turn_only =
            state.cfg.include_contents == IncludeContents::None || placement_hides_history;
        // How to shape the turn honors the declaration too (see `mode_for`).
        let is_single_turn = mode_for(ctx, &*agent) == Mode::SingleTurn;
        // A compaction record tells prompt assembly to drop a span of history and substitute
        // content in its place. Tool code and plugins can write EventActions, so records are only
        // honored when this run actually has compaction configured.
        let compaction_enabled = runtime::configured(ctx);
        let events: Vec<Event> = ctx
            .session()
            .events()
            .into_iter()
            .filter(|e| compaction_enabled || e.actions.compaction.is_none())
            .collect();
        let (branch, scope) = (ctx.branch(), ctx.isolation_scope());
        let contents = if current_turn_only {
            build_contents_current_turn_context_only(
                &name,
                &branch,
                &scope,
                &events,
                is_single_turn,
                ctx.user_content(),
            )?
        } else {
            build_contents_default(
                &name,
                &branch,
                &scope,
                &events,
                is_single_turn,
                ctx.user_content(),
            )?
        };
        req.contents.extend(contents);
        // If the history concludes on a model turn, inject a synthetic user continuation turn
        // so the model keeps producing output rather than returning an empty response.
        if req.contents.last().is_some_and(|c| c.role != "user") {
            req.contents
                .push(Content::from_text(CONTINUE_PROCESSING, "user"));
        }
        Ok(Vec::new())
    })
}

/// The request contents after filtering,
/// rearrangement and content processing of `events`.
pub(crate) fn build_contents_default(
    agent_name: &str,
    branch: &str,
    isolation_scope: &str,
    events: &[Event],
    is_single_turn: bool,
    user_content: Option<&Content>,
) -> Result<Vec<Content>, AgentError> {
    build_contents_default_with_call_source(
        agent_name,
        branch,
        isolation_scope,
        events,
        events,
        is_single_turn,
        user_content,
    )
}

/// As [`build_contents_default`], with the calls that responses pair with taken from
/// `all_events`.
pub(crate) fn build_contents_default_with_call_source(
    agent_name: &str,
    branch: &str,
    isolation_scope: &str,
    events: &[Event],
    all_events: &[Event],
    is_single_turn: bool,
    user_content: Option<&Content>,
) -> Result<Vec<Content>, AgentError> {
    // Keep the contents and the function calls and responses visible to the current agent.
    let mut filtered: Vec<Event> = Vec::new();
    let call_ids = function_call_ids(all_events);
    // Whether the agent sees `ev` at all: it has content (or a transcription or compaction
    // summary), and it is on the agent's branch, not on a tool sub-branch below it, in its
    // isolation scope (an exact match), and not excluded. Compaction events are exempt from the
    // content check: they carry their summary on the compaction record, and `apply` below
    // expands them into content.
    let visible = |ev: &Event| {
        !is_empty_event(ev)
            && event_belongs_to_branch(branch, &ev.branch)
            && !is_tool_sub_branch_event(branch, ev, agent_name, &call_ids)
            && ev.isolation_scope == isolation_scope
            && !should_exclude_event(ev)
    };
    // The author of the latest visible call with each ID, filled in as the events are read so a
    // response pairs with the call before it. When `events` is a suffix of
    // `all_events`, as for the compaction token estimate, the calls before the suffix are
    // recorded first, so a response in the suffix pairs the same way it does when the whole
    // session is built.
    let mut call_authors: HashMap<String, String> = HashMap::new();
    if let Some(first) = events.first()
        && let Some(start) = all_events
            .iter()
            .position(|e| std::ptr::eq(e, first) || e == first)
    {
        for ev in &all_events[..start] {
            if visible(ev) {
                record_call_authors(&mut call_authors, ev);
            }
        }
    }
    for ev in events {
        if !visible(ev) {
            continue;
        }
        // A function response to a call another agent made is that agent's exchange, so it is
        // presented as context like the call itself, as in adk-python's _get_contents. A
        // response pairs with the latest visible call before it, so a later call that reuses
        // the ID does not turn the agent's own finished exchange into context.
        let foreign = is_other_agent_reply(agent_name, ev)
            || answers_other_agent_call(agent_name, ev, &call_authors);
        record_call_authors(&mut call_authors, ev);
        if foreign && !has_usable_summary(ev) {
            // None signals the foreign event is dropped (e.g. a thought-only turn).
            if let Some(converted) = convert_foreign_event(ev) {
                filtered.push(converted);
            }
        } else {
            filtered.push(ev.clone());
        }
    }

    // Replace each compaction summary with the events it covers.
    // A no-op when no record reached here: records are dropped at collection unless compaction
    // is configured for the run.
    let filtered = apply(&filtered);

    // Aggregate transcription events (convert to text parts on the fly).
    let mut processed: Vec<Event> = Vec::with_capacity(filtered.len());
    let (mut acc_input, mut acc_output) = (String::new(), String::new());
    let mut iter = filtered.into_iter().peekable();
    while let Some(mut ev) = iter.next() {
        if content(&ev).is_none_or(|c| c.parts.is_empty()) {
            let has_text =
                |t: Option<&gadk_genai::Transcription>| t.is_some_and(|t| !t.text.is_empty());
            if has_text(ev.response.input_transcription.as_ref()) {
                if let Some(t) = &ev.response.input_transcription {
                    acc_input.push_str(&t.text);
                }
                if iter
                    .peek()
                    .is_some_and(|next| has_text(next.response.input_transcription.as_ref()))
                {
                    continue;
                }
                ev.response.input_transcription = None;
                ev.response.content = Some(Content {
                    role: "user".into(),
                    parts: vec![text_part(std::mem::take(&mut acc_input))],
                });
            } else if has_text(ev.response.output_transcription.as_ref()) {
                if let Some(t) = &ev.response.output_transcription {
                    acc_output.push_str(&t.text);
                }
                if iter
                    .peek()
                    .is_some_and(|next| has_text(next.response.output_transcription.as_ref()))
                {
                    continue;
                }
                ev.response.output_transcription = None;
                ev.response.content = Some(Content {
                    role: "model".into(),
                    parts: vec![text_part(std::mem::take(&mut acc_output))],
                });
            }
        }
        processed.push(ev);
    }

    let (filtered, orphan_remnants) = drop_orphaned_function_responses(processed, all_events);
    // adk-python _rearrange_events_for_async_function_response.
    let filtered = rearrange_events_for_latest_function_response(filtered, &orphan_remnants)?;
    // adk-python _rearrange_events_for_async_function_responses_in_history.
    let filtered = rearrange_events_for_function_responses_in_history(filtered)?;
    // Runs after both rearrangements: dropping a trailing unanswered call first would leave a
    // function response as the last event and let the first rearrangement discard the turns
    // between it and its call.
    let filtered = drop_orphaned_function_calls(filtered);

    let mut contents = Vec::with_capacity(filtered.len());
    for ev in filtered {
        let Some(mut c) = ev.response.content else {
            continue;
        };
        // Gemini 3 in streaming returns a last response with an empty part; filter it out. The
        // live audio cache records its blobs as artifact:// refs that are ADK bookkeeping, not
        // conversation content; replaying one fails the request.
        c.parts
            .retain(|p| *p != Part::default() && !is_internal_live_artifact_part(p));
        if c.parts.is_empty() {
            continue;
        }
        remove_client_function_call_id(&mut c);
        contents.push(c);
    }

    // For scoped agents (task / single_turn), prepend a synthetic user content built from the
    // originating FC's args, re-derived from the PRE-filter events.
    if !isolation_scope.is_empty()
        && let Some(leading) =
            build_task_input_user_content(events, isolation_scope, is_single_turn, user_content)
    {
        contents.insert(0, leading);
    }
    Ok(contents)
}

fn text_part(text: String) -> Part {
    Part {
        text,
        ..Default::default()
    }
}

/// Drops function responses with no matching call. Returns the pruned events
/// plus the INDICES (into the returned Vec) of the surviving events whose orphaned responses
/// were removed, so later rearrangement preserves their remaining content.
pub(crate) fn drop_orphaned_function_responses(
    events: Vec<Event>,
    all_events: &[Event],
) -> (Vec<Event>, HashSet<usize>) {
    let call_ids: HashSet<&str> = all_events
        .iter()
        .flat_map(|e| function_calls(content(e)))
        .filter(|c| !c.id.is_empty())
        .map(|c| c.id.as_str())
        .collect();
    let is_orphan = |p: &Part| {
        p.function_response
            .as_ref()
            .is_some_and(|fr| !fr.id.is_empty() && !call_ids.contains(fr.id.as_str()))
    };

    let mut orphaned_ids: Vec<String> = Vec::new();
    let mut result = Vec::with_capacity(events.len());
    let mut orphan_remnants = HashSet::new();
    for mut event in events {
        let Some(c) = event.response.content.as_mut() else {
            result.push(event);
            continue;
        };
        if !c.parts.iter().any(is_orphan) {
            result.push(event);
            continue;
        }
        for mut part in std::mem::take(&mut c.parts) {
            if is_orphan(&part) {
                if let Some(fr) = part.function_response.take() {
                    orphaned_ids.push(fr.id);
                }
                if part == Part::default() {
                    continue;
                }
            }
            c.parts.push(part);
        }
        if !c.parts.is_empty() {
            orphan_remnants.insert(result.len());
            result.push(event);
        }
    }

    if !orphaned_ids.is_empty() {
        tracing::warn!(
            target: "adk",
            "adk: dropping function responses with no matching function call: {}",
            go_quote_list(&orphaned_ids)
        );
    }
    (result, orphan_remnants)
}

/// Removes function calls that no
/// function response in `events` answers. A call without an ID, or listed in an event's
/// long-running IDs, is kept. The whole part goes (thought signature included); an event left
/// with no parts is removed.
pub(crate) fn drop_orphaned_function_calls(events: Vec<Event>) -> Vec<Event> {
    let mut answered: HashSet<String> = HashSet::new();
    for event in &events {
        for r in function_responses(content(event)) {
            if !r.id.is_empty() {
                answered.insert(r.id.clone());
            }
        }
        answered.extend(event.long_running_tool_ids.iter().cloned());
    }
    let is_orphan = |p: &Part| {
        p.function_call
            .as_ref()
            .is_some_and(|fc| !fc.id.is_empty() && !answered.contains(&fc.id))
    };

    let mut orphaned_ids: Vec<String> = Vec::new();
    let mut result = Vec::with_capacity(events.len());
    for mut event in events {
        let Some(c) = event.response.content.as_mut() else {
            result.push(event);
            continue;
        };
        if !c.parts.iter().any(is_orphan) {
            result.push(event);
            continue;
        }
        for part in std::mem::take(&mut c.parts) {
            if is_orphan(&part) {
                if let Some(fc) = part.function_call {
                    orphaned_ids.push(fc.id);
                }
                continue;
            }
            c.parts.push(part);
        }
        if !c.parts.is_empty() {
            result.push(event);
        }
    }

    if !orphaned_ids.is_empty() {
        tracing::warn!(
            target: "adk",
            "adk: dropping function calls with no matching function response: {}",
            go_quote_list(&orphaned_ids)
        );
    }
    result
}

/// An id set formatted as `[a, b]`, ids sorted (the form used in error messages).
fn id_list(ids: &BTreeSet<String>) -> String {
    format!(
        "[{}]",
        ids.iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(", ")
    )
}

/// Merges responses
/// to the latest event's matching call while preserving unrelated tool events and the orphan
/// remnants (indices into `events`). Other intervening non-tool events are dropped.
pub(crate) fn rearrange_events_for_latest_function_response(
    events: Vec<Event>,
    orphan_remnants: &HashSet<usize>,
) -> Result<Vec<Event>, AgentError> {
    if events.len() < 2 {
        return Ok(events);
    }
    let last = events.len() - 1;
    let last_responses = function_responses(content(&events[last]));
    // Nothing to do when the latest event is not a function response.
    if last_responses.is_empty() {
        return Ok(events);
    }
    let mut response_ids: BTreeSet<String> = last_responses.iter().map(|r| r.id.clone()).collect();

    // Already in the correct position: the preceding event carries a matching call.
    if function_calls(content(&events[last - 1]))
        .iter()
        .any(|c| response_ids.contains(&c.id))
    {
        return Ok(events);
    }

    let mut call_event_idx = None;
    for idx in (0..last).rev() {
        let calls = function_calls(content(&events[idx]));
        if !calls.iter().any(|c| response_ids.contains(&c.id)) {
            continue;
        }
        let all_call_ids: BTreeSet<String> = calls.iter().map(|c| c.id.clone()).collect();
        // The last response event may only answer calls of this same call event.
        if !response_ids.is_subset(&all_call_ids) {
            return Err(AgentError::other(format!(
                "validation failed: last response event has IDs not in the matching call event. Call IDs: {}, Response IDs: {}",
                id_list(&all_call_ids),
                id_list(&response_ids)
            )));
        }
        response_ids = all_call_ids;
        call_event_idx = Some(idx);
        break;
    }
    let Some(call_event_idx) = call_event_idx else {
        return Err(AgentError::other(format!(
            "no function call event found for function response ids: {}",
            id_list(&response_ids)
        )));
    };

    let mut to_merge = Vec::new();
    let mut result = Vec::with_capacity(events.len());
    for (i, event) in events.into_iter().enumerate() {
        if i <= call_event_idx {
            result.push(event);
            continue;
        }
        if i == last {
            to_merge.push(event);
            continue;
        }
        let c = content(&event);
        if !function_calls(c).is_empty() {
            result.push(event);
            continue;
        }
        let responses = function_responses(c);
        if responses.is_empty() {
            // Retain content deliberately preserved when pruning unrelated orphaned responses.
            if orphan_remnants.contains(&i) {
                result.push(event);
            }
            continue;
        }
        let related = responses.iter().any(|r| response_ids.contains(&r.id));
        if related {
            to_merge.push(event);
        } else {
            result.push(event);
        }
    }
    result.push(merge_function_response_events(&to_merge)?);
    Ok(result)
}

/// Rearranges the history so every
/// function call event is immediately followed by one consolidated response event, and the
/// pair(s) answered by the last event are emitted last.
pub(crate) fn rearrange_events_for_function_responses_in_history(
    events: Vec<Event>,
) -> Result<Vec<Event>, AgentError> {
    if events.len() < 2 {
        return Ok(events);
    }
    // Call ID → index of the event holding its response (last writer wins).
    let mut response_event_index: HashMap<&str, usize> = HashMap::new();
    for (i, event) in events.iter().enumerate() {
        for r in function_responses(content(event)) {
            response_event_index.insert(r.id.as_str(), i);
        }
    }
    let last = events.len() - 1;
    let last_response_event_idx =
        (!function_responses(content(&events[last])).is_empty()).then_some(last);

    let (mut result, mut tail) = (Vec::new(), Vec::new());
    for event in &events {
        // Response events are handled with their call event.
        if !function_responses(content(event)).is_empty() {
            continue;
        }
        let calls = function_calls(content(event));
        if calls.is_empty() {
            result.push(event.clone());
            continue;
        }
        let mut pair = vec![event.clone()];
        let indices: BTreeSet<usize> = calls
            .iter()
            .filter_map(|c| response_event_index.get(c.id.as_str()).copied())
            .collect();
        match indices.len() {
            // A long running tool or a deferring tool produces a call with no response event.
            0 => {}
            1 => pair.extend(indices.iter().map(|&i| events[i].clone())),
            _ => {
                let to_merge: Vec<Event> = indices.iter().map(|&i| events[i].clone()).collect();
                pair.push(
                    merge_function_response_events(&to_merge)
                        .map_err(|e| wrap_err("failed to merge response events", e))?,
                );
            }
        }
        if last_response_event_idx.is_some_and(|l| indices.contains(&l)) {
            tail.extend(pair);
        } else {
            result.extend(pair);
        }
    }
    result.extend(tail);
    Ok(result)
}

/// Merges function response
/// events into one. Later function response parts replace earlier ones with the same ID; other
/// parts are appended.
pub(crate) fn merge_function_response_events(events: &[Event]) -> Result<Event, AgentError> {
    let Some(first) = events.first() else {
        return Err(AgentError::other(
            "at least one function_response event is required",
        ));
    };
    let mut merged = first.clone();
    let Some(merged_content) = merged.response.content.as_mut() else {
        return Err(AgentError::other("content for the first event is missing"));
    };
    if merged_content.parts.is_empty() {
        return Err(AgentError::other(
            "there should be at least one function_response part",
        ));
    }
    let mut index_by_id: HashMap<String, usize> = HashMap::new();
    for (i, p) in merged_content.parts.iter().enumerate() {
        if let Some(fr) = &p.function_response {
            index_by_id.insert(fr.id.clone(), i);
        }
    }
    for event in &events[1..] {
        let parts = content(event)
            .map(|c| c.parts.as_slice())
            .unwrap_or_default();
        if parts.is_empty() {
            return Err(AgentError::other("event should contain at least one part"));
        }
        for part in parts {
            match &part.function_response {
                Some(fr) => match index_by_id.get(&fr.id) {
                    Some(&i) => merged_content.parts[i] = part.clone(),
                    None => {
                        merged_content.parts.push(part.clone());
                        index_by_id.insert(fr.id.clone(), merged_content.parts.len() - 1);
                    }
                },
                None => merged_content.parts.push(part.clone()),
            }
        }
    }
    Ok(merged)
}

/// Contents for the
/// current turn only, which starts at the latest visible event from the user or another agent.
///
/// A tool result the caller posted back (a long-running tool's, say) is not a turn start, and
/// the turn reaches back to the call it answers, past any user turn sent while the tool was
/// pending. A `transfer_to_agent` hand-over continues the turn of the request it
/// hands over, so it does not start the turn either; with no earlier turn start,
/// the turn starts at the latest hand-over. An event with nothing to read or on a
/// tool's sub-branch cannot start the turn.
pub(crate) fn build_contents_current_turn_context_only(
    agent_name: &str,
    branch: &str,
    isolation_scope: &str,
    events: &[Event],
    is_single_turn: bool,
    user_content: Option<&Content>,
) -> Result<Vec<Content>, AgentError> {
    // The IDs of every call in the session, an empty one included: full-history mode pairs a
    // result and a call that both lack an ID, so this mode tracks them too.
    let all_call_ids: HashSet<&str> = events
        .iter()
        .flat_map(|e| function_calls(content(e)))
        .map(|c| c.id.as_str())
        .collect();
    let sub_branch_call_ids = function_call_ids(events);
    let mut unanswered: HashSet<&str> = HashSet::new();
    // The latest hand-over skipped as a turn start below, if any.
    let mut handover: Option<usize> = None;
    for i in (0..events.len()).rev() {
        let event = &events[i];
        for call in function_calls(content(event)) {
            unanswered.remove(call.id.as_str());
        }
        // A sibling branch's event is not part of this agent's current turn.
        if !event_belongs_to_branch(branch, &event.branch) {
            continue;
        }
        // A tool's sub-branch event is skipped in the contents, so it cannot start the turn
        // either: it would cut off the call and the response that came before it.
        if is_tool_sub_branch_event(branch, event, agent_name, &sub_branch_call_ids) {
            continue;
        }
        // An out-of-scope event is invisible to the agent, so it cannot start its turn.
        if event.isolation_scope != isolation_scope {
            continue;
        }
        if is_submitted_tool_result(event) {
            for r in function_responses(content(event)) {
                if all_call_ids.contains(r.id.as_str()) {
                    unanswered.insert(r.id.as_str());
                }
            }
            continue;
        }
        if !unanswered.is_empty() {
            continue;
        }
        // An event that gives the model nothing to read is left out of the contents, so it
        // cannot start the turn either: it would slice out the input before it.
        if is_empty_event(event) {
            continue;
        }
        let foreign = is_other_agent_reply(agent_name, event);
        // An event discarded by foreign conversion cannot start a visible turn.
        if foreign && !has_usable_summary(event) && convert_foreign_event(event).is_none() {
            continue;
        }
        if event.author != "user" && !foreign {
            continue;
        }
        if is_direct_transfer(event) {
            handover.get_or_insert(i);
            continue;
        }
        return build_contents_default_with_call_source(
            agent_name,
            branch,
            isolation_scope,
            &events[i..],
            events,
            is_single_turn,
            user_content,
        );
    }
    // With no earlier turn start, the turn starts at the latest hand-over. Falling back to the
    // whole session instead would show the agent its own earlier turns.
    if let Some(h) = handover {
        return build_contents_default_with_call_source(
            agent_name,
            branch,
            isolation_scope,
            &events[h..],
            events,
            is_single_turn,
            user_content,
        );
    }
    build_contents_default(
        agent_name,
        branch,
        isolation_scope,
        events,
        is_single_turn,
        user_content,
    )
}

/// `ev` gives the model nothing to read (no content
/// with a role and parts, no transcription and no usable compaction summary).
fn is_empty_event(ev: &Event) -> bool {
    content(ev).is_none_or(|c| c.role.is_empty() || c.parts.is_empty())
        && ev.response.input_transcription.is_none()
        && ev.response.output_transcription.is_none()
        && !has_usable_summary(ev)
}

/// A tool result the caller posted back as
/// a user-authored function response.
fn is_submitted_tool_result(ev: &Event) -> bool {
    ev.author == "user" && !function_responses(content(ev)).is_empty()
}

/// A `transfer_to_agent` call or the response
/// that carries out the transfer.
fn is_direct_transfer(ev: &Event) -> bool {
    !ev.actions.transfer_to_agent.is_empty()
        || function_calls(content(ev))
            .iter()
            .any(|c| c.name == TRANSFER_TO_AGENT_TOOL_NAME)
}

/// Whether `ev` is a reply from an agent other than the current one or the user.
fn is_other_agent_reply(current_agent_name: &str, ev: &Event) -> bool {
    ev.author != current_agent_name && ev.author != "user"
}

/// Records `ev`'s author as the author of each function
/// call with an ID in `ev`. A call without one is skipped, so a response without an ID never
/// matches it.
fn record_call_authors(authors: &mut HashMap<String, String>, ev: &Event) {
    for call in function_calls(content(ev)) {
        if !call.id.is_empty() {
            authors.insert(call.id.clone(), ev.author.clone());
        }
    }
}

/// `ev` holds a function response to a call
/// that an agent other than `current_agent_name` made.
fn answers_other_agent_call(
    current_agent_name: &str,
    ev: &Event,
    call_authors: &HashMap<String, String>,
) -> bool {
    function_responses(content(ev)).iter().any(|r| {
        call_authors
            .get(&r.id)
            .is_some_and(|a| a != current_agent_name && a != "user")
    })
}

/// The non-empty IDs of the function calls in
/// `events`.
pub(crate) fn function_call_ids(events: &[Event]) -> HashSet<String> {
    events
        .iter()
        .flat_map(|e| function_calls(content(e)))
        .filter(|c| !c.id.is_empty())
        .map(|c| c.id.clone())
        .collect()
}

/// `ev` was published by a tool
/// on a sub-branch below `agent_branch`, such as a single_turn sub-agent run on
/// `<agent>@<function_call_id>`. The calling agent gets the tool's function response, so these
/// events are not part of its context.
///
/// Every segment below `agent_branch` is checked, so events nested further down a tool's
/// sub-branch are skipped too. A segment naming `agent_name` is the agent's own branch and is
/// not skipped. A segment keyed by a run counter is a workflow node's branch and is not skipped
/// either, unless a function call in the session has an ID equal to that counter.
pub(crate) fn is_tool_sub_branch_event(
    agent_branch: &str,
    ev: &Event,
    agent_name: &str,
    call_ids: &HashSet<String>,
) -> bool {
    if ev.branch.is_empty() {
        return false;
    }
    let base: Vec<&str> = if agent_branch.is_empty() {
        Vec::new()
    } else {
        agent_branch.split('.').collect()
    };
    let segments: Vec<&str> = ev.branch.split('.').collect();
    if segments.len() <= base.len() || segments[..base.len()] != base[..] {
        return false;
    }
    segments[base.len()..].iter().any(|segment| {
        segment
            .rfind('@')
            .is_some_and(|i| call_ids.contains(&segment[i + 1..]) && &segment[..i] != agent_name)
    })
}

/// Stringifies a function-call args or function-response map as compact JSON.
///
/// A Rust `Map` has no null form, so an empty map always renders `{}`: Gemini sends
/// `"args": {}` for a no-argument call, and tool results (transfer_to_agent returns an empty
/// map) are maps too.
fn stringify_map(m: &Map<String, Value>) -> String {
    go_json_marshal(&Value::Object(m.clone()))
}

/// Presents another agent's event as
/// user context. Thought parts, internal live-artifact refs and standalone metadata
/// are omitted. Returns `Some(ev.clone())` for a content-less event and `None` to
/// drop it (a foreign turn with no remaining payload).
pub(crate) fn convert_foreign_event(ev: &Event) -> Option<Event> {
    let Some(c) = content(ev).filter(|c| !c.parts.is_empty()) else {
        return Some(ev.clone());
    };
    let mut parts = vec![text_part("For context:".into())];
    for p in &c.parts {
        // Never replay another agent's private reasoning.
        if p.thought {
            continue;
        }
        // The live audio cache authors its blob events as the agent that spoke. Skipping the
        // refs here, rather than only after conversion, lets an event whose only parts are
        // internal refs convert to `None` instead of a bare "For context:" turn.
        if is_internal_live_artifact_part(p) {
            continue;
        }
        if !p.text.is_empty() {
            parts.push(text_part(format!("[{}] said: {}", ev.author, p.text)));
        } else if let Some(fc) = &p.function_call {
            parts.push(text_part(format!(
                "[{}] called tool `{}` with parameters: {}",
                ev.author,
                fc.name,
                stringify_map(&fc.args)
            )));
        } else if let Some(fr) = &p.function_response {
            // An empty result map renders as `{}` (see stringify_map).
            parts.push(text_part(format!(
                "[{}] `{}` tool returned result: {}",
                ev.author,
                fr.name,
                stringify_map(&fr.response)
            )));
        } else if p.inline_data.is_some()
            || p.file_data.is_some()
            || p.executable_code.is_some()
            || p.code_execution_result.is_some()
            || p.tool_call.is_some()
            || p.tool_response.is_some()
            || p.audio_transcription.is_some()
        {
            // Keep server-tool and transcript parts alongside the media/code payloads.
            // Standalone metadata has no relay payload.
            parts.push(p.clone());
        }
    }
    // Only the header remains (e.g. a thought-only foreign turn): drop the event.
    if parts.len() == 1 {
        return None;
    }
    // A made-up event; the invocation ID comes along because compaction identifies an event by
    // (InvocationID, Timestamp).
    Some(Event {
        timestamp: ev.timestamp,
        author: "user".into(),
        invocation_id: ev.invocation_id.clone(),
        branch: ev.branch.clone(),
        response: LlmResponse {
            content: Some(Content {
                role: "user".into(),
                parts,
            }),
            ..Default::default()
        },
        ..Default::default()
    })
}

/// Credential and confirmation requests
/// and responses never reach the model.
pub(crate) fn should_exclude_event(ev: &Event) -> bool {
    let excluded = |name: &str| {
        name == REQUEST_EUC_FUNCTION_CALL_NAME || name == REQUEST_CONFIRMATION_FUNCTION_CALL_NAME
    };
    content(ev).is_some_and(|c| {
        c.parts.iter().any(|p| {
            p.function_call
                .as_ref()
                .is_some_and(|fc| excluded(&fc.name))
                || p.function_response
                    .as_ref()
                    .is_some_and(|fr| excluded(&fr.name))
        })
    })
}

/// The first turn of a scoped
/// (task / single_turn) agent, from the args of the delegating function call whose ID is the
/// isolation scope, else from `user_content`. Appends [`SINGLE_TURN_NUDGE`] when single-turn.
pub(crate) fn build_task_input_user_content(
    events: &[Event],
    isolation_scope: &str,
    is_single_turn: bool,
    user_content: Option<&Content>,
) -> Option<Content> {
    if isolation_scope.is_empty() {
        return None;
    }
    let nudge = || text_part(SINGLE_TURN_NUDGE.into());
    let delegating_call = events
        .iter()
        .filter_map(content)
        .flat_map(|c| c.parts.iter())
        .filter_map(|p| p.function_call.as_ref())
        .find(|fc| fc.id == isolation_scope && !fc.args.is_empty());
    if let Some(fc) = delegating_call {
        // Render args as JSON: the same shape an LLM would emit.
        let mut parts = vec![text_part(go_json_marshal(&Value::Object(fc.args.clone())))];
        if is_single_turn {
            parts.push(nudge());
        }
        return Some(Content {
            role: "user".into(),
            parts,
        });
    }
    let user_content = user_content.filter(|c| !c.parts.is_empty())?;
    let mut parts = user_content.parts.clone();
    if is_single_turn {
        parts.push(nudge());
    }
    Some(Content {
        role: "user".into(),
        parts,
    })
}

/// Removes `display_name` from file data: the Gemini API (non-Vertex)
/// backend rejects `display_name` on file uploads, so it is removed.
pub(crate) fn remove_display_name_if_exists<'a>(
    ctx: &'a InvocationContext,
    req: &'a mut LlmRequest,
    _f: &'a Flow,
) -> BoxFuture<'a, Result<Vec<Event>, AgentError>> {
    Box::pin(async move {
        if req.contents.is_empty() {
            return Ok(Vec::new());
        }
        let agent = ctx.agent();
        let Some(state) = reveal(&*agent) else {
            return Ok(Vec::new());
        };
        let Some(model) = state.cfg.model.as_deref() else {
            return Ok(Vec::new());
        };
        if !gadk_model::is_gemini_api_variant(model) {
            return Ok(Vec::new());
        }
        for part in req.contents.iter_mut().flat_map(|c| c.parts.iter_mut()) {
            if let Some(d) = part.inline_data.as_mut() {
                d.display_name.clear();
            }
            if let Some(d) = part.file_data.as_mut() {
                d.display_name.clear();
            }
        }
        Ok(Vec::new())
    })
}

#[cfg(test)]
mod tests {
    use gadk_genai::{FunctionCall, FunctionResponse};
    use gadk_session::{CreateRequest, GetRequest, InMemorySessionService, SessionService};
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::*;
    use crate::llm::{config::LlmAgentConfig, processors::test_llm_agent};

    const AGENT: &str = "me";

    fn ev(author: &str, role: &str, parts: Vec<Part>) -> Event {
        Event {
            author: author.into(),
            response: LlmResponse {
                content: Some(Content {
                    role: role.into(),
                    parts,
                }),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn call(id: &str, name: &str, args: Value) -> Part {
        Part {
            function_call: Some(FunctionCall {
                id: id.into(),
                name: name.into(),
                args: args.as_object().cloned().unwrap_or_default(),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn response(id: &str, name: &str) -> Part {
        Part {
            function_response: Some(FunctionResponse {
                id: id.into(),
                name: name.into(),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn texts(c: &Content) -> Vec<&str> {
        c.parts.iter().map(|p| p.text.as_str()).collect()
    }

    fn build(events: &[Event]) -> Vec<Content> {
        build_contents_default(AGENT, "", "", events, false, None).unwrap()
    }

    #[tokio::test]
    async fn continue_processing_appended_after_model_turn() {
        let svc = InMemorySessionService::new();
        let created = svc
            .create(CreateRequest {
                app_name: "app".into(),
                user_id: "user".into(),
                session_id: Some("s".into()),
                state: None,
            })
            .await
            .unwrap();
        for mut e in [
            ev("user", "user", vec![text_part("hi".into())]),
            ev(AGENT, "model", vec![text_part("hello".into())]),
        ] {
            svc.append_event(&*created, &mut e).await.unwrap();
        }
        let sess = svc
            .get(GetRequest {
                app_name: "app".into(),
                user_id: "user".into(),
                session_id: "s".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        let agent = test_llm_agent(LlmAgentConfig {
            name: AGENT.into(),
            ..Default::default()
        });
        let ctx = InvocationContext::builder()
            .agent(agent)
            .session(sess)
            .build_invocation();
        let mut req = LlmRequest::default();
        contents_request_processor(&ctx, &mut req, &Flow::default())
            .await
            .unwrap();
        assert_eq!(req.contents.len(), 3);
        assert_eq!(
            req.contents.last().unwrap(),
            &Content::from_text(CONTINUE_PROCESSING, "user")
        );
    }

    #[test]
    fn foreign_text_rewritten_for_context() {
        let events = vec![ev(
            "other",
            "model",
            vec![text_part("hi".into()), call("", "t", json!({"a": "<b>"}))],
        )];
        let got = build(&events);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].role, "user");
        assert_eq!(
            texts(&got[0]),
            [
                "For context:",
                "[other] said: hi",
                "[other] called tool `t` with parameters: {\"a\":\"\\u003cb\\u003e\"}"
            ]
        );
    }

    #[test]
    fn foreign_empty_args_and_empty_response_render_object() {
        let events = vec![ev(
            "other",
            "model",
            vec![call("", "t", json!({})), response("", "t")],
        )];
        let got = build(&events);
        assert_eq!(
            texts(&got[0]),
            [
                "For context:",
                "[other] called tool `t` with parameters: {}",
                "[other] `t` tool returned result: {}"
            ]
        );
    }

    /// A foreign no-argument call as Gemini sends it (`"args": {}`) renders `{}`, as Go does.
    #[test]
    fn foreign_no_arg_function_call_renders_empty_object() {
        let events = vec![ev("other", "model", vec![call("", "confirm", json!({}))])];
        let got = build(&events);
        assert_eq!(
            texts(&got[0]),
            [
                "For context:",
                "[other] called tool `confirm` with parameters: {}"
            ]
        );
    }

    #[test]
    fn client_function_call_ids_removed() {
        let events = vec![
            ev(AGENT, "model", vec![call("adk-123", "t", json!({}))]),
            ev(AGENT, "user", vec![response("adk-123", "t")]),
            ev(AGENT, "model", vec![call("call_1", "t", json!({}))]),
            ev(AGENT, "user", vec![response("call_1", "t")]),
        ];
        let got = build(&events);
        assert_eq!(got.len(), 4);
        assert_eq!(got[0].parts[0].function_call.as_ref().unwrap().id, "");
        assert_eq!(got[1].parts[0].function_response.as_ref().unwrap().id, "");
        assert_eq!(got[2].parts[0].function_call.as_ref().unwrap().id, "call_1");
        assert_eq!(
            got[3].parts[0].function_response.as_ref().unwrap().id,
            "call_1"
        );
    }

    #[test]
    fn confirmation_events_excluded() {
        let events = vec![
            ev("user", "user", vec![text_part("hi".into())]),
            ev(
                AGENT,
                "model",
                vec![call(
                    "c1",
                    REQUEST_CONFIRMATION_FUNCTION_CALL_NAME,
                    json!({"x": 1}),
                )],
            ),
        ];
        let got = build(&events);
        assert_eq!(got, vec![Content::from_text("hi", "user")]);
    }

    #[test]
    fn thought_only_foreign_event_dropped() {
        let events = vec![
            ev("user", "user", vec![text_part("hi".into())]),
            ev(
                "other",
                "model",
                vec![Part {
                    text: "secret reasoning".into(),
                    thought: true,
                    ..Default::default()
                }],
            ),
        ];
        assert_eq!(convert_foreign_event(&events[1]), None);
        assert_eq!(build(&events), vec![Content::from_text("hi", "user")]);
    }

    #[test]
    fn scoped_agent_gets_task_input_first() {
        let mut parent = ev(
            "coordinator",
            "model",
            vec![call("fc-1", "worker", json!({"task": "<do it>"}))],
        );
        parent.isolation_scope = String::new();
        let got = build_contents_default(AGENT, "", "fc-1", &[parent], true, None).unwrap();
        assert_eq!(
            texts(&got[0]),
            ["{\"task\":\"\\u003cdo it\\u003e\"}", SINGLE_TURN_NUDGE]
        );
    }

    #[test]
    fn latest_response_merged_after_its_call() {
        let events = vec![
            ev(
                AGENT,
                "model",
                vec![call("a", "t", json!({})), call("b", "t", json!({}))],
            ),
            ev(AGENT, "user", vec![response("a", "t")]),
            ev("user", "user", vec![text_part("still there?".into())]),
            ev(AGENT, "user", vec![response("b", "t")]),
        ];
        let got = build(&events);
        assert_eq!(got.len(), 2);
        let ids: Vec<_> = got[1]
            .parts
            .iter()
            .map(|p| p.function_response.as_ref().unwrap().id.as_str())
            .collect();
        assert_eq!(ids, ["a", "b"]);
    }

    #[test]
    fn latest_response_with_no_call_event_errors() {
        let events = vec![
            ev(AGENT, "model", vec![call("a", "t", json!({}))]),
            ev("user", "user", vec![text_part("x".into())]),
            ev(AGENT, "user", vec![response("z", "t"), response("y", "t")]),
        ];
        let err =
            rearrange_events_for_latest_function_response(events, &HashSet::new()).unwrap_err();
        assert_eq!(
            err.to_string(),
            "no function call event found for function response ids: [y, z]"
        );
    }

    #[test]
    fn latest_response_without_call_errors() {
        let events = vec![
            ev(AGENT, "model", vec![call("a", "t", json!({}))]),
            ev(AGENT, "user", vec![response("a", "t")]),
            ev(
                AGENT,
                "model",
                vec![call("b", "t", json!({})), call("c", "t", json!({}))],
            ),
            ev("user", "user", vec![text_part("x".into())]),
            ev(AGENT, "user", vec![response("b", "t"), response("a", "t")]),
        ];
        let err =
            rearrange_events_for_latest_function_response(events, &HashSet::new()).unwrap_err();
        assert_eq!(
            err.to_string(),
            "validation failed: last response event has IDs not in the matching call event. Call IDs: [b, c], Response IDs: [a, b]"
        );
    }
}
