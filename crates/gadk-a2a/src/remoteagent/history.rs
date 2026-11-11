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

//! Remote-session history parts, isolation scope and user function-call mapping (port of ADK-Go
//! `agent/remoteagent/v2/utils.go`).

use std::collections::HashSet;

use gadk_agent::InvocationContext;
use gadk_genai::{Content, FunctionResponse, Part, ROLE_USER};
use gadk_platform::go_sprint_value;
use gadk_session::Event;
use serde_json::{Map, Value};

use super::agent::A2AConfig;
use crate::Error;
use crate::convert::{get_a2a_task_info, to_a2a_part};

/// Go `plugininternal.BeforeRunReplyKey` (gadk-engine's crate-private
/// `runner::BEFORE_RUN_REPLY_KEY`; `convert::metadata` keeps another private copy).
pub(crate) const BEFORE_RUN_REPLY_KEY: &str = "adk:before_run_reply";

/// Whether the event is a BeforeRun reply (Go `event.CustomMetadata[BeforeRunReplyKey] == true`).
fn is_before_run_reply(event: &Event) -> bool {
    event.response.custom_metadata.get(BEFORE_RUN_REPLY_KEY) == Some(&Value::Bool(true))
}

/// Go `userFunctionCall`: the user event answering a function call, and the A2A task and context
/// IDs recorded on the call's event.
#[derive(Debug, Clone)]
pub(crate) struct UserFunctionCall {
    /// The user event carrying the function response.
    pub(crate) response: Event,
    /// The A2A task ID of the function-call event.
    pub(crate) task_id: String,
    /// The A2A context ID of the function-call event.
    pub(crate) context_id: String,
}

/// Go `getUserFunctionCallAt`: `Some` when the event at `index` is a user function response
/// answering a call this peer (`agent_name`; any author when empty) made in `scope`. A call
/// produced by a BeforeRun callback does not resume the named peer.
pub(crate) fn get_user_function_call_at(
    events: &[Event],
    index: isize,
    agent_name: &str,
    scope: &str,
) -> Option<UserFunctionCall> {
    let index = usize::try_from(index).ok()?;
    let candidate = events.get(index)?;
    if candidate.author != "user" {
        return None;
    }
    let fn_call_id = get_function_response_call_id(candidate)?;
    for request in events[..index].iter().rev() {
        if request.isolation_scope != scope
            || !is_function_call_event(request, &fn_call_id, agent_name)
        {
            continue;
        }
        if !agent_name.is_empty() && is_before_run_reply(request) {
            return None;
        }
        let (task_id, context_id) = get_a2a_task_info(request);
        return Some(UserFunctionCall {
            response: candidate.clone(),
            task_id,
            context_id,
        });
    }
    None
}

/// Go `isFunctionCallEvent`: whether `event` (authored by `agent_name`, any author when empty)
/// holds a function call with ID `call_id`.
fn is_function_call_event(event: &Event, call_id: &str, agent_name: &str) -> bool {
    let Some(content) = &event.response.content else {
        return false;
    };
    if call_id.is_empty() {
        return false;
    }
    // Empty agentName skips the author gate (anonymous wrappers / harnesses).
    if !agent_name.is_empty() && event.author != agent_name {
        return false;
    }
    content.parts.iter().any(|part| {
        part.function_call
            .as_ref()
            .is_some_and(|fc| fc.id == call_id)
    })
}

/// Go `collectRemoteFunctionCallIDs`: the call IDs this peer itself emitted in `scope` (any author
/// when `agent_name` is empty). A later locally produced call (a BeforeRun reply) reusing an ID
/// takes it back.
pub(crate) fn collect_remote_function_call_ids(
    events: &[Event],
    agent_name: &str,
    scope: &str,
) -> HashSet<String> {
    let mut ids = HashSet::new();
    for event in events {
        let Some(content) = &event.response.content else {
            continue;
        };
        if event.isolation_scope != scope {
            continue;
        }
        // Empty agentName skips the author gate (anonymous wrappers / harnesses).
        if !agent_name.is_empty() && event.author != agent_name {
            continue;
        }
        let remote_call = agent_name.is_empty() || !is_before_run_reply(event);
        for part in &content.parts {
            if let Some(fc) = &part.function_call
                && !fc.id.is_empty()
            {
                // A cache can reuse an older call's ID. The latest call owns the response, even
                // when that call was produced locally.
                if remote_call {
                    ids.insert(fc.id.clone());
                } else {
                    ids.remove(&fc.id);
                }
            }
        }
    }
    ids
}

/// Go `marshalFunctionResponseJSON`: the payload as Go's `json.Encoder` with
/// `SetEscapeHTML(false)` writes it (sorted keys, Go float format, `<`, `>` and `&` kept
/// literal; U+2028 and U+2029 still escaped); `<unserializable>` when it cannot be encoded.
pub(crate) fn marshal_function_response_json(v: &Map<String, Value>) -> String {
    let Ok(bytes) = gadk_model::to_go_json(v) else {
        return "<unserializable>".to_owned();
    };
    let Ok(text) = String::from_utf8(bytes) else {
        return "<unserializable>".to_owned();
    };
    unescape_html(&text)
}

/// Undoes Go `json.Marshal`'s HTML escaping (`\u003c`, `\u003e`, `\u0026`) in JSON text. Every
/// backslash in JSON text starts an escape sequence, so the scan pairs each one with what it
/// escapes and only rewrites the three HTML escapes.
fn unescape_html(json: &str) -> String {
    let mut out = String::with_capacity(json.len());
    let mut rest = json;
    while let Some(i) = rest.find('\\') {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let replaced = [("\\u003c", '<'), ("\\u003e", '>'), ("\\u0026", '&')]
            .iter()
            .find(|(esc, _)| tail.starts_with(esc));
        if let Some((esc, c)) = replaced {
            out.push(*c);
            rest = &tail[esc.len()..];
        } else {
            // Copy the backslash and the character it escapes.
            let mut chars = tail.char_indices();
            chars.next();
            let end = chars.next().map_or(tail.len(), |(j, c)| j + c.len_utf8());
            out.push_str(&tail[..end]);
            rest = &tail[end..];
        }
    }
    out.push_str(rest);
    out
}

/// Go `unmatchedFunctionResponseText`.
pub(crate) fn unmatched_function_response_text(fr: &FunctionResponse) -> String {
    let name = if fr.name.is_empty() {
        "<unnamed>"
    } else {
        fr.name.as_str()
    };
    format!(
        "Tool {name} returned: {}",
        marshal_function_response_json(&fr.response)
    )
}

/// Go `getFunctionResponseCallID`: the call ID of the event's first function response.
fn get_function_response_call_id(event: &Event) -> Option<String> {
    event
        .response
        .content
        .as_ref()?
        .parts
        .iter()
        .find_map(|part| part.function_response.as_ref())
        .map(|fr| fr.id.clone())
}

/// Go `toMissingRemoteSessionParts`: the parts of every in-scope event after this agent's last
/// reply (BeforeRun replies excepted), which the remote session has not seen, and the A2A
/// context ID recorded on that reply (`""` when there is none).
pub(crate) async fn to_missing_remote_session_parts(
    ctx: &InvocationContext,
    events: &[Event],
    cfg: &A2AConfig,
) -> (Vec<a2a::Part>, String) {
    let agent = ctx.agent();
    let agent_name = agent.name();
    let scope = ctx.isolation_scope();
    let mut context_id = String::new();
    // only events after this index are not in the remote session
    let mut first_missing = 0;
    for (i, event) in events.iter().enumerate().rev() {
        // Isolation scopes require an exact match, so an unscoped invocation replays only
        // unscoped events.
        if event.isolation_scope != scope {
            continue;
        }
        // Only BeforeRun replies are skipped: other callbacks keep their history boundaries.
        if event.author == agent_name && !is_before_run_reply(event) {
            first_missing = i + 1;
            context_id = get_a2a_task_info(event).1;
            break;
        }
    }

    let remote_fc_ids = collect_remote_function_call_ids(events, agent_name, &scope);
    let mut result = Vec::new();
    for (i, event) in events.iter().enumerate().skip(first_missing) {
        // Same exact-match rule as above.
        if event.isolation_scope != scope {
            continue;
        }
        // BeforeRun replies, like other agents' replies, are context the peer has not seen.
        // Render their calls as text rather than peer calls. Keep anonymous wrappers' existing
        // attribution behavior.
        let presented;
        let event = if !agent_name.is_empty()
            && event.author != "user"
            && (event.author != agent_name || is_before_run_reply(event))
        {
            presented = present_as_user_message(ctx, event);
            &presented
        } else {
            event
        };
        if event
            .response
            .content
            .as_ref()
            .is_none_or(|c| c.parts.is_empty())
        {
            continue;
        }
        match convert_parts(cfg, event, Some(&remote_fc_ids)).await {
            Ok(parts) => result.extend(parts),
            Err(err) => {
                tracing::warn!(index = i, error = %err, "failed to convert parts for session event");
            }
        }
    }
    (result, context_id)
}

/// Go `hasIsolationScopeHistory`: whether the session already holds an event in `scope`.
pub(crate) fn has_isolation_scope_history(events: &[Event], scope: &str) -> bool {
    events.iter().any(|e| e.isolation_scope == scope)
}

/// Go `presentAsUserMessage`: another agent's event retold as user context (`For context:` then
/// one text part per part; thoughts dropped). No content when nothing remains to tell.
pub(crate) fn present_as_user_message(ctx: &InvocationContext, agent_event: &Event) -> Event {
    let mut event = Event::new(ctx.platform(), ctx.invocation_id());
    event.author = "user".to_owned();

    let Some(content) = &agent_event.response.content else {
        return event;
    };
    let author = &agent_event.author;
    let mut parts = Vec::with_capacity(content.parts.len() + 1);
    parts.push(Part::from_text("For context:"));
    for part in &content.parts {
        if part.thought {
            continue;
        }
        if !part.text.is_empty() {
            parts.push(Part::from_text(format!("[{author}] said: {}", part.text)));
        } else if let Some(call) = &part.function_call {
            parts.push(Part::from_text(format!(
                "[{author}] called tool {} with parameters: {}",
                call.name,
                go_sprint_value(&Value::Object(call.args.clone()))
            )));
        } else if let Some(resp) = &part.function_response {
            parts.push(Part::from_text(format!(
                "[{author}] {} tool returned result: {}",
                resp.name,
                go_sprint_value(&Value::Object(resp.response.clone()))
            )));
        } else {
            parts.push(part.clone());
        }
    }
    if parts.len() > 1 {
        // not only "For context:" part
        event.response.content = Some(Content::from_parts(parts, ROLE_USER));
    }
    event
}

/// Go `convertParts`: the event's parts as A2A parts. With `remote_fc_ids` (history path), a
/// function response whose ID is not in the set is rewritten as text; `None` (resume path)
/// rewrites nothing. A `genai_part_converter` replaces the default conversion; its `None` drops
/// the part.
pub(crate) async fn convert_parts(
    cfg: &A2AConfig,
    event: &Event,
    remote_fc_ids: Option<&HashSet<String>>,
) -> Result<Vec<a2a::Part>, Error> {
    let Some(content) = &event.response.content else {
        return Ok(Vec::new());
    };
    let mut parts = Vec::with_capacity(content.parts.len());
    for part in &content.parts {
        if let (Some(ids), Some(fr)) = (remote_fc_ids, &part.function_response)
            && !ids.contains(&fr.id)
        {
            let text = unmatched_function_response_text(fr);
            tracing::warn!(
                tool = %fr.name,
                id = %fr.id,
                "rewrote unmatched function response as text for remote peer"
            );
            parts.push(a2a::Part::text(text));
            continue;
        }
        if let Some(converter) = &cfg.genai_part_converter {
            if let Some(converted) = converter(Some(event), part).await? {
                parts.push(converted);
            }
            continue;
        }
        let converted = to_a2a_part(part, &event.long_running_tool_ids)
            .map_err(|e| Error::wrap("event part conversion failed", e))?;
        parts.push(converted);
    }
    Ok(parts)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn unescape_html_keeps_other_escapes() {
        let v = json!({"a": "<b>&\"\\u003c\u{2028}", "n": 1.5});
        let Value::Object(m) = v else { unreachable!() };
        assert_eq!(
            marshal_function_response_json(&m),
            r#"{"a":"<b>&\"\\u003c\u2028","n":1.5}"#
        );
    }
}
