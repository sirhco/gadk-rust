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

//! Event conversions (port of ADK-Go `server/adka2a/v2/events.go` and `utils.go`
//! `WithoutPartialArtifacts`).

use std::collections::HashMap;

use gadk_agent::InvocationContext;
use gadk_genai::{Content, Part, ROLE_MODEL};
use gadk_session::Event;
use serde_json::{Map, Value};

use super::metadata::{
    CUSTOM_META_CONTEXT_ID_KEY, CUSTOM_META_TASK_ID_KEY, METADATA_IS_ERR_MESSAGE_KEY,
    METADATA_PARTIAL_KEY, artifact_update_visible_meta, process_a2a_meta, process_meta_view,
    set_actions_meta, to_event_meta,
};
use super::parts::{
    DATA_PART_META_LONG_RUNNING_KEY, is_partial, part_meta, to_a2a_parts, to_genai_part,
    to_genai_role,
};
use super::{A2APartConverter, StreamResponseRef};
use crate::Error;

/// Go `NewRemoteAgentEvent`: a new event authored by the agent running in `ctx`, on its branch.
pub fn new_remote_agent_event(ctx: &InvocationContext) -> Event {
    let mut event = Event::new(ctx.platform(), ctx.invocation_id());
    event.author = ctx.agent().name().to_owned();
    event.branch = ctx.branch();
    event
}

/// Go `EventToMessage`: the session event as an A2A message (`agent` role unless the author is
/// `user`). Always `Some`: Go's `nil` result is only for a `nil` event.
pub fn event_to_message(event: &Event) -> Result<Option<a2a::Message>, Error> {
    let event_parts = event
        .response
        .content
        .as_ref()
        .map_or(&[][..], |c| &c.parts[..]);
    let parts = to_a2a_parts(event_parts, &event.long_running_tool_ids)
        .map_err(|e| Error::wrap("part conversion failed", e))?;
    let event_meta = to_event_meta(&HashMap::new(), event)
        .map_err(|e| Error::wrap("event metadata conversion failed", e))?;

    let role = if event.author == "user" {
        a2a::Role::User
    } else {
        a2a::Role::Agent
    };
    let mut msg = a2a::Message::new(role, parts);
    let mut meta = HashMap::new();
    set_actions_meta(&mut meta, &event.actions);
    meta.extend(event_meta);
    msg.metadata = (!meta.is_empty()).then_some(meta);
    Ok(Some(msg))
}

/// Go `ToSessionEvent`: the A2A event as a session event authored by the agent running in `ctx`;
/// `None` when the event carries nothing to record.
pub async fn to_session_event(
    ctx: &InvocationContext,
    event: &a2a::StreamResponse,
) -> Result<Option<Event>, Error> {
    to_session_event_with_parts(ctx, event, None).await
}

/// Go `ToSessionEventWithParts`: [`to_session_event`] with a custom part converter (`None` uses
/// [`to_genai_part`]).
pub async fn to_session_event_with_parts(
    ctx: &InvocationContext,
    event: &a2a::StreamResponse,
    converter: Option<&A2APartConverter>,
) -> Result<Option<Event>, Error> {
    match event {
        a2a::StreamResponse::Task(task) => task_to_event(ctx, event, task, converter).await,
        a2a::StreamResponse::Message(msg) => message_to_event(ctx, msg, converter).await.map(Some),
        a2a::StreamResponse::ArtifactUpdate(update) => {
            if update.artifact.parts.is_empty() {
                return Ok(None);
            }
            if is_partial(update.metadata.as_ref()) && update.last_chunk.unwrap_or(false) {
                // Partial ADK artifact reset.
                return Ok(None);
            }
            let mut result = artifact_update_event_to_event(ctx, event, update, converter)
                .await
                .map_err(|e| Error::wrap("artifact update event conversion failed", e))?;
            if content_is_empty(&result) {
                return Ok(None);
            }
            process_a2a_meta(event, &mut result)
                .map_err(|e| Error::wrap("metadata processing failed", e))?;
            let visible = artifact_update_visible_meta(update);
            match visible.as_ref().and_then(|m| m.get(METADATA_PARTIAL_KEY)) {
                Some(Value::Bool(partial)) => result.response.partial = *partial,
                // append=false, lastChunk=false: emitted as partial, caller restarts aggregation
                // append=false, lastChunk=true: emitted as non partial, caller drops aggregation
                // append=true, lastChunk=false: emitted as partial, caller updates aggregation
                // append=true, lastChunk=true: emitted as partial, caller updates and emits
                // aggregation as non-partial
                _ => {
                    result.response.partial =
                        update.append.unwrap_or(false) || !update.last_chunk.unwrap_or(false);
                }
            }
            Ok(Some(result))
        }
        a2a::StreamResponse::StatusUpdate(update) => {
            let state = &update.status.state;
            if state.is_terminal() || *state == a2a::TaskState::InputRequired {
                return final_task_status_update_to_event(ctx, event, update, converter)
                    .await
                    .map(Some);
            }
            let Some(msg) = &update.status.message else {
                return Ok(None);
            };
            let mut result = message_to_event(ctx, msg, converter)
                .await
                .map_err(|e| Error::wrap("custom metadata conversion failed", e))?;
            result.response.turn_complete = false;
            // Go dereferences a nil content here (a panic); a message without converted parts
            // is skipped instead.
            if content_is_empty(&result) {
                return Ok(None);
            }
            process_a2a_meta(event, &mut result)
                .map_err(|e| Error::wrap("metadata processing failed", e))?;
            if let Some(content) = &mut result.response.content {
                for part in &mut content.parts {
                    if !part.text.is_empty() {
                        part.thought = true;
                    }
                }
            }
            result.response.partial = true;
            Ok(Some(result))
        }
    }
}

/// Go `ToCustomMetadata`: session event custom metadata holding the A2A task and context IDs
/// (empty when both are empty, Go's `nil`).
pub fn to_custom_metadata(task_id: &str, context_id: &str) -> Map<String, Value> {
    let mut result = Map::new();
    if !task_id.is_empty() {
        result.insert(
            CUSTOM_META_TASK_ID_KEY.to_owned(),
            Value::String(task_id.to_owned()),
        );
    }
    if !context_id.is_empty() {
        result.insert(
            CUSTOM_META_CONTEXT_ID_KEY.to_owned(),
            Value::String(context_id.to_owned()),
        );
    }
    result
}

/// Go `GetA2ATaskInfo`: the `(task_id, context_id)` stored in the event's custom metadata (`""`
/// when absent).
pub fn get_a2a_task_info(event: &Event) -> (String, String) {
    let get = |key: &str| match event.response.custom_metadata.get(key) {
        Some(Value::String(s)) => s.clone(),
        _ => String::new(),
    };
    (
        get(CUSTOM_META_TASK_ID_KEY),
        get(CUSTOM_META_CONTEXT_ID_KEY),
    )
}

/// Go `WithoutPartialArtifacts`: drops the partial artifacts (usually discarded, part-less,
/// after the invocation finished).
pub fn without_partial_artifacts(artifacts: Vec<a2a::Artifact>) -> Vec<a2a::Artifact> {
    artifacts
        .into_iter()
        .filter(|artifact| !is_partial(artifact.metadata.as_ref()))
        .collect()
}

/// Whether the event has no content or no parts (Go `len(event.Content.Parts) == 0`).
fn content_is_empty(event: &Event) -> bool {
    event
        .response
        .content
        .as_ref()
        .is_none_or(|c| c.parts.is_empty())
}

/// Model-role content of `parts`.
fn model_content(parts: Vec<Part>) -> Content {
    Content {
        parts,
        role: ROLE_MODEL.to_owned(),
    }
}

/// One part through the converter, or [`to_genai_part`] without one.
async fn convert_part(
    converter: Option<&A2APartConverter>,
    source: StreamResponseRef<'_>,
    part: &a2a::Part,
) -> Result<Option<Part>, Error> {
    match converter {
        Some(converter) => converter(source, part).await,
        None => to_genai_part(part).map(Some),
    }
}

/// Go `messageToEvent`.
async fn message_to_event(
    ctx: &InvocationContext,
    msg: &a2a::Message,
    converter: Option<&A2APartConverter>,
) -> Result<Event, Error> {
    let mut parts = Vec::new();
    for part in &msg.parts {
        let converted = convert_part(converter, StreamResponseRef::Message(msg), part)
            .await
            .map_err(|e| Error::wrap("failed to convert message part", e))?;
        parts.extend(converted);
    }

    let mut event = new_remote_agent_event(ctx);
    if !parts.is_empty() {
        event.response.content = Some(Content {
            parts,
            role: to_genai_role(&msg.role).to_owned(),
        });
    }
    process_meta_view(
        msg.task_id.as_deref().unwrap_or_default(),
        msg.context_id.as_deref().unwrap_or_default(),
        msg.metadata.as_ref(),
        None,
        &mut event,
    )
    .map_err(|e| Error::wrap("metadata processing failed", e))?;
    event.response.turn_complete = true;
    Ok(event)
}

/// Go `artifactUpdateEventToEvent`.
async fn artifact_update_event_to_event(
    ctx: &InvocationContext,
    source: &a2a::StreamResponse,
    update: &a2a::TaskArtifactUpdateEvent,
    converter: Option<&A2APartConverter>,
) -> Result<Event, Error> {
    let all_parts = convert_parts(source, &update.artifact.parts, converter)
        .await
        .map_err(|e| Error::wrap("failed to convert artifact parts", e))?;

    let mut event = new_remote_agent_event(ctx);
    event.long_running_tool_ids = get_long_running_tool_ids(&update.artifact.parts, &all_parts);
    event.response.content = Some(model_content(all_parts.into_iter().flatten().collect()));
    Ok(event)
}

/// Go `taskToEvent`.
async fn task_to_event(
    ctx: &InvocationContext,
    source: &a2a::StreamResponse,
    task: &a2a::Task,
    converter: Option<&A2APartConverter>,
) -> Result<Option<Event>, Error> {
    let mut parts = Vec::new();
    let mut long_running_tool_ids = Vec::new();
    for artifact in task.artifacts.iter().flatten() {
        let all_parts = convert_parts(source, &artifact.parts, converter)
            .await
            .map_err(|e| Error::wrap("failed to convert artifact parts", e))?;
        long_running_tool_ids.extend(get_long_running_tool_ids(&artifact.parts, &all_parts));
        parts.extend(all_parts.into_iter().flatten());
    }

    let mut event = new_remote_agent_event(ctx);

    let status = convert_status_message(source, &task.status, converter).await?;
    parts.extend(status.parts);
    long_running_tool_ids.extend(status.long_running_tool_ids);

    if task.status.state == a2a::TaskState::Failed {
        event.response.error_message = if status.error_message.is_empty() {
            "a2a task failed".to_owned()
        } else {
            status.error_message
        };
    }

    let is_terminal =
        task.status.state.is_terminal() || task.status.state == a2a::TaskState::InputRequired;
    if parts.is_empty() && !is_terminal {
        return Ok(None);
    }
    if !parts.is_empty() {
        event.response.content = Some(model_content(parts));
    }
    if task.status.state == a2a::TaskState::InputRequired {
        event.long_running_tool_ids = long_running_tool_ids;
    }
    process_a2a_meta(source, &mut event)
        .map_err(|e| Error::wrap("metadata processing failed", e))?;
    event.response.turn_complete = is_terminal;
    Ok(Some(event))
}

/// Go `finalTaskStatusUpdateToEvent`.
async fn final_task_status_update_to_event(
    ctx: &InvocationContext,
    source: &a2a::StreamResponse,
    update: &a2a::TaskStatusUpdateEvent,
    converter: Option<&A2APartConverter>,
) -> Result<Event, Error> {
    let mut event = new_remote_agent_event(ctx);

    let status = convert_status_message(source, &update.status, converter).await?;
    if !status.parts.is_empty() {
        event.response.content = Some(model_content(status.parts));
    }
    event.long_running_tool_ids = status.long_running_tool_ids;

    if update.status.state == a2a::TaskState::Failed {
        event.response.error_message = if status.error_message.is_empty() {
            "a2a task failed".to_owned()
        } else {
            status.error_message
        };
    }

    process_a2a_meta(source, &mut event)
        .map_err(|e| Error::wrap("metadata processing failed", e))?;
    event.response.turn_complete = true;
    Ok(event)
}

/// Go `getLongRunningToolIDs`: the IDs of the function calls converted from data parts marked
/// long-running.
fn get_long_running_tool_ids(parts: &[a2a::Part], converted: &[Option<Part>]) -> Vec<String> {
    let mut ids = Vec::new();
    for (i, part) in parts.iter().enumerate() {
        if !matches!(&part.content, a2a::PartContent::Data(data) if !data.is_null()) {
            continue;
        }
        if !matches!(
            part_meta(part, DATA_PART_META_LONG_RUNNING_KEY),
            Some(Value::Bool(true))
        ) {
            continue;
        }
        let Some(Some(converted)) = converted.get(i) else {
            continue;
        };
        // Go: TODO(yarolegovich): log a warning when a long-running part is not a function call.
        if let Some(call) = &converted.function_call {
            ids.push(call.id.clone());
        }
    }
    ids
}

/// Go `convertParts`: converts A2A parts keeping index alignment (a converter's `Ok(None)` stays
/// a `None` entry).
async fn convert_parts(
    source: &a2a::StreamResponse,
    parts: &[a2a::Part],
    converter: Option<&A2APartConverter>,
) -> Result<Vec<Option<Part>>, Error> {
    let mut result = Vec::with_capacity(parts.len());
    for part in parts {
        let converted = convert_part(converter, StreamResponseRef::Event(source), part)
            .await
            .map_err(|e| Error::wrap("failed to convert part", e))?;
        result.push(converted);
    }
    Ok(result)
}

/// Go `convertedStatusMessage`.
#[derive(Default)]
struct ConvertedStatusMessage {
    error_message: String,
    parts: Vec<Part>,
    long_running_tool_ids: Vec<String>,
}

/// Go `convertStatusMessage`: the status message's parts; for a failed task, the first text part
/// is the error message and is dropped from the parts when flagged `adk_is_error_message`.
async fn convert_status_message(
    source: &a2a::StreamResponse,
    status: &a2a::TaskStatus,
    converter: Option<&A2APartConverter>,
) -> Result<ConvertedStatusMessage, Error> {
    let Some(msg) = &status.message else {
        return Ok(ConvertedStatusMessage::default());
    };
    let mut error_message = String::new();
    let mut parts = &msg.parts[..];
    if status.state == a2a::TaskState::Failed
        && let Some(first) = parts.first()
        && let Some(text) = first.as_text().filter(|t| !t.is_empty())
    {
        error_message = text.to_owned();
        if matches!(
            part_meta(first, METADATA_IS_ERR_MESSAGE_KEY),
            Some(Value::Bool(true))
        ) {
            parts = &parts[1..];
        }
    }

    let converted = convert_parts(source, parts, converter)
        .await
        .map_err(|e| Error::wrap("failed to convert status message parts", e))?;
    Ok(ConvertedStatusMessage {
        error_message,
        long_running_tool_ids: get_long_running_tool_ids(parts, &converted),
        parts: converted.into_iter().flatten().collect(),
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use gadk_agent::{AgentConfig, ContextBuilder, new_agent};
    use gadk_session::EventActions;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::*;

    fn meta(v: Value) -> Option<HashMap<String, Value>> {
        match v {
            Value::Object(m) => Some(m.into_iter().collect()),
            _ => None,
        }
    }

    fn artifact_update(
        append: Option<bool>,
        last_chunk: Option<bool>,
        metadata: Option<HashMap<String, Value>>,
        artifact_metadata: Option<HashMap<String, Value>>,
    ) -> a2a::StreamResponse {
        a2a::StreamResponse::ArtifactUpdate(a2a::TaskArtifactUpdateEvent {
            task_id: "t".into(),
            context_id: "c".into(),
            artifact: a2a::Artifact {
                artifact_id: "a".into(),
                name: None,
                description: None,
                parts: vec![a2a::Part::text("x")],
                metadata: artifact_metadata,
                extensions: None,
            },
            append,
            last_chunk,
            metadata,
        })
    }

    /// Go `ToSessionEventWithParts` for artifact updates: the ADK partial flag wins; without it
    /// the event is partial unless it is a non-append last chunk; a partial last chunk is the
    /// ADK partial-artifact reset and yields nothing.
    #[tokio::test]
    async fn artifact_update_partial_follows_go() {
        let agent = new_agent(AgentConfig {
            name: "a".into(),
            ..Default::default()
        })
        .unwrap();
        let ctx = ContextBuilder::new().agent(agent).build_invocation();
        let partial = |p: bool| meta(json!({ (METADATA_PARTIAL_KEY): p }));
        let cases = [
            (artifact_update(None, None, None, None), Some(true)),
            (artifact_update(None, Some(true), None, None), Some(false)),
            (
                artifact_update(Some(true), Some(true), None, None),
                Some(true),
            ),
            (
                artifact_update(None, None, partial(false), None),
                Some(false),
            ),
            (artifact_update(None, Some(true), partial(true), None), None),
            // Go merges the artifact's metadata into the event's own map in place, so the flag
            // is seen only when the event has a map of its own.
            (
                artifact_update(None, Some(true), meta(json!({})), partial(true)),
                Some(true),
            ),
            (
                artifact_update(None, Some(true), None, partial(true)),
                Some(false),
            ),
        ];
        for (i, (input, want)) in cases.into_iter().enumerate() {
            let got = to_session_event(&ctx, &input).await.unwrap();
            assert_eq!(got.map(|e| e.response.partial), want, "case {i}");
        }
    }

    #[test]
    fn event_to_message_carries_role_parts_and_metadata() {
        let event = Event {
            response: gadk_model::LlmResponse {
                content: Some(Content {
                    parts: vec![Part {
                        text: "hi".into(),
                        ..Default::default()
                    }],
                    role: ROLE_MODEL.into(),
                }),
                error_code: "E".into(),
                ..Default::default()
            },
            invocation_id: "inv".into(),
            author: "agent".into(),
            actions: EventActions {
                escalate: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let msg = event_to_message(&event).unwrap().unwrap();
        assert_eq!(msg.role, a2a::Role::Agent);
        assert_eq!(msg.parts, vec![a2a::Part::text("hi")]);
        assert_eq!(
            serde_json::to_value(&msg.metadata).unwrap(),
            json!({
                "adk_invocation_id": "inv",
                "adk_author": "agent",
                "adk_error_code": "E",
                "adk_escalate": true,
            })
        );

        let user = Event {
            author: "user".into(),
            ..Default::default()
        };
        let msg = event_to_message(&user).unwrap().unwrap();
        assert_eq!((msg.role, msg.parts), (a2a::Role::User, vec![]));
        assert_eq!(msg.metadata, meta(json!({"adk_author": "user"})));

        // Go keeps a nil metadata map when there is nothing to write.
        let msg = event_to_message(&Event::default()).unwrap().unwrap();
        assert_eq!(msg.metadata, None);
    }

    #[test]
    fn task_info_round_trips_through_custom_metadata() {
        let event = Event {
            response: gadk_model::LlmResponse {
                custom_metadata: to_custom_metadata("t", "c"),
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(get_a2a_task_info(&event), ("t".into(), "c".into()));
        assert!(to_custom_metadata("", "").is_empty());
        assert_eq!(
            get_a2a_task_info(&Event::default()),
            (String::new(), String::new())
        );
    }

    #[test]
    fn without_partial_artifacts_drops_partial_ones() {
        let artifact = |id: &str, metadata| a2a::Artifact {
            artifact_id: id.into(),
            name: None,
            description: None,
            parts: vec![],
            metadata,
            extensions: None,
        };
        let kept = without_partial_artifacts(vec![
            artifact("a", None),
            artifact("b", meta(json!({ (METADATA_PARTIAL_KEY): true }))),
            artifact("c", meta(json!({ (METADATA_PARTIAL_KEY): false }))),
        ]);
        let ids: Vec<_> = kept.iter().map(|a| a.artifact_id.as_str()).collect();
        assert_eq!(ids, ["a", "c"]);
    }
}
