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

//! Metadata conversions between session events and A2A metadata maps. Reading the invocation
//! metadata from the a2a-rs request context belongs to the executor; [`to_event_meta`] takes the
//! resulting invocation metadata map as `base`.

use std::collections::HashMap;

use gadk_engine::runner::BEFORE_RUN_REPLY_KEY;
use gadk_session::{Event, EventActions};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

use super::events::to_custom_metadata;
use crate::Error;

/// The session event custom-metadata key of the A2A task ID.
pub(crate) const CUSTOM_META_TASK_ID_KEY: &str = "a2a:task_id";
/// The session event custom-metadata key of the A2A context ID.
pub(crate) const CUSTOM_META_CONTEXT_ID_KEY: &str = "a2a:context_id";

/// A2A metadata key of the `escalate` action.
pub(crate) const METADATA_ESCALATE_KEY: &str = "adk_escalate";
/// A2A metadata key of the `transfer_to_agent` action.
pub(crate) const METADATA_TRANSFER_TO_AGENT_KEY: &str = "adk_transfer_to_agent";
/// A2A metadata key of the event's error code.
pub(crate) const METADATA_ERROR_CODE_KEY: &str = "adk_error_code";
/// A2A metadata key of the citation metadata.
pub(crate) const METADATA_CITATION_KEY: &str = "adk_citation_metadata";
/// A2A metadata key of the grounding metadata.
pub(crate) const METADATA_GROUNDING_KEY: &str = "adk_grounding_metadata";
/// A2A metadata key of the usage metadata.
pub(crate) const METADATA_USAGE_KEY: &str = "adk_usage_metadata";
/// A2A metadata key of the event's custom metadata.
pub(crate) const METADATA_CUSTOM_META_KEY: &str = "adk_custom_metadata";
/// A2A metadata key of the partial flag.
pub(crate) const METADATA_PARTIAL_KEY: &str = "adk_partial";
/// A2A metadata key marking a failed task's status text part as the error message.
pub(crate) const METADATA_IS_ERR_MESSAGE_KEY: &str = "adk_is_error_message";

/// Adds the prefix that marks ADK values in A2A metadata.
pub fn to_a2a_meta_key(key: &str) -> String {
    format!("adk_{key}")
}

/// Adds the prefix that marks A2A values in session event custom metadata.
pub fn to_adk_meta_key(key: &str) -> String {
    format!("a2a:{key}")
}

/// The `transfer_to_agent` value a remote peer set on its response
/// metadata. The conversions never restore it (it drives the local orchestrator's control flow);
/// a caller that trusts the peer restores it explicitly with this.
pub fn transfer_to_agent_from_meta(meta: Option<&HashMap<String, Value>>) -> Option<String> {
    match meta?.get(METADATA_TRANSFER_TO_AGENT_KEY)? {
        Value::String(name) => Some(name.clone()),
        _ => None,
    }
}

/// Only `escalate` is restored from peer metadata; `transfer_to_agent` is
/// intentionally not.
pub(crate) fn to_event_actions(meta: Option<&HashMap<String, Value>>) -> EventActions {
    let escalate = matches!(
        meta.and_then(|m| m.get(METADATA_ESCALATE_KEY)),
        Some(Value::Bool(true))
    );
    EventActions {
        escalate,
        ..Default::default()
    }
}

/// The A2A metadata of `event`: `base` (the invocation metadata; empty for
/// [`super::event_to_message`]) plus the event's identifiers, citation, grounding, usage and custom
/// metadata and error code.
pub(crate) fn to_event_meta(
    base: &HashMap<String, Value>,
    event: &Event,
) -> Result<HashMap<String, Value>, Error> {
    let mut result = base.clone();
    for (key, value) in [
        ("invocation_id", &event.invocation_id),
        ("author", &event.author),
        ("branch", &event.branch),
    ] {
        if !value.is_empty() {
            result.insert(to_a2a_meta_key(key), Value::String(value.clone()));
        }
    }
    let response = &event.response;
    add_meta(
        &mut result,
        METADATA_CITATION_KEY,
        response.citation_metadata.as_ref(),
    )?;
    add_meta(
        &mut result,
        METADATA_GROUNDING_KEY,
        response.grounding_metadata.as_ref(),
    )?;
    add_meta(
        &mut result,
        METADATA_USAGE_KEY,
        response.usage_metadata.as_ref(),
    )?;
    if !response.custom_metadata.is_empty() {
        result.insert(
            METADATA_CUSTOM_META_KEY.to_owned(),
            Value::Object(response.custom_metadata.clone()),
        );
    }
    if !response.error_code.is_empty() {
        result.insert(
            METADATA_ERROR_CODE_KEY.to_owned(),
            Value::String(response.error_code.clone()),
        );
    }
    Ok(result)
}

/// Writes `escalate` and `transfer_to_agent`; nothing is written when neither is set. Callers
/// keep `None` for an empty map.
pub(crate) fn set_actions_meta(meta: &mut HashMap<String, Value>, actions: &EventActions) {
    if actions.escalate {
        meta.insert(METADATA_ESCALATE_KEY.to_owned(), Value::Bool(true));
    }
    if !actions.transfer_to_agent.is_empty() {
        meta.insert(
            METADATA_TRANSFER_TO_AGENT_KEY.to_owned(),
            Value::String(actions.transfer_to_agent.clone()),
        );
    }
}

/// Restores citation, grounding and usage metadata, the custom metadata
/// (with the A2A task and context IDs), the error code and the actions from an A2A event.
pub(crate) fn process_a2a_meta(
    a2a_event: &a2a::StreamResponse,
    event: &mut Event,
) -> Result<(), Error> {
    let (task_id, context_id) = stream_task_info(a2a_event);
    let artifact_meta = match a2a_event {
        a2a::StreamResponse::ArtifactUpdate(update) => update.artifact.metadata.as_ref(),
        _ => None,
    };
    process_meta_view(
        task_id,
        context_id,
        stream_meta(a2a_event),
        artifact_meta,
        event,
    )
}

/// [`process_a2a_meta`] over the pieces of an A2A event: its task info, its own metadata and, for
/// an artifact update, the artifact's metadata. Lets a bare message (a task status message) be
/// processed without building a [`a2a::StreamResponse`].
pub(crate) fn process_meta_view(
    task_id: &str,
    context_id: &str,
    own_meta: Option<&HashMap<String, Value>>,
    artifact_meta: Option<&HashMap<String, Value>>,
    event: &mut Event,
) -> Result<(), Error> {
    let merged = merge_artifact_meta(own_meta, artifact_meta);
    let meta = merged.as_ref().or(own_meta);

    process_meta(
        METADATA_CITATION_KEY,
        meta,
        &mut event.response.citation_metadata,
    )?;
    process_meta(
        METADATA_GROUNDING_KEY,
        meta,
        &mut event.response.grounding_metadata,
    )?;
    process_meta(METADATA_USAGE_KEY, meta, &mut event.response.usage_metadata)?;

    event.response.custom_metadata = to_custom_metadata(task_id, context_id);
    if let Some(Value::Object(user_meta)) = meta.and_then(|m| m.get(METADATA_CUSTOM_META_KEY)) {
        // Through JSON text, so protobuf-decoded integral floats (`12.0`) are integers again.
        let user_meta: Map<String, Value> = from_map_structure(user_meta)?;
        event.response.custom_metadata.extend(user_meta);
        event.response.custom_metadata.remove(BEFORE_RUN_REPLY_KEY);
    }

    if let Some(Value::String(code)) = meta.and_then(|m| m.get(METADATA_ERROR_CODE_KEY)) {
        event.response.error_code = code.clone();
    }

    // When the event has its own metadata map, the artifact metadata is merged into it, so the
    // actions are read from the merged keys; without an own map the actions see none.
    let actions_meta = if own_meta.is_some() { meta } else { None };
    event.actions = to_event_actions(actions_meta);
    Ok(())
}

/// The metadata [`process_a2a_meta`] leaves visible on an artifact update: the update's own map
/// with the artifact's metadata merged in, or `None` when the update has no map of its own. The
/// partial flag of an artifact update is read from this view.
pub(crate) fn artifact_update_visible_meta(
    update: &a2a::TaskArtifactUpdateEvent,
) -> Option<HashMap<String, Value>> {
    let own = update.metadata.as_ref()?;
    Some(
        merge_artifact_meta(Some(own), update.artifact.metadata.as_ref())
            .unwrap_or_else(|| own.clone()),
    )
}

/// The event's own metadata with a non-empty artifact metadata map copied over it; `None` when
/// there is nothing to merge.
fn merge_artifact_meta(
    own_meta: Option<&HashMap<String, Value>>,
    artifact_meta: Option<&HashMap<String, Value>>,
) -> Option<HashMap<String, Value>> {
    let artifact_meta = artifact_meta.filter(|m| !m.is_empty())?;
    let mut merged = own_meta.cloned().unwrap_or_default();
    merged.extend(artifact_meta.iter().map(|(k, v)| (k.clone(), v.clone())));
    Some(merged)
}

/// `(task_id, context_id)`; a message without them yields `""`.
pub(crate) fn stream_task_info(event: &a2a::StreamResponse) -> (&str, &str) {
    match event {
        a2a::StreamResponse::Task(task) => (&task.id, &task.context_id),
        a2a::StreamResponse::Message(msg) => (
            msg.task_id.as_deref().unwrap_or_default(),
            msg.context_id.as_deref().unwrap_or_default(),
        ),
        a2a::StreamResponse::StatusUpdate(update) => (&update.task_id, &update.context_id),
        a2a::StreamResponse::ArtifactUpdate(update) => (&update.task_id, &update.context_id),
    }
}

/// The metadata map of the stream event, whichever variant it is.
pub(crate) fn stream_meta(event: &a2a::StreamResponse) -> Option<&HashMap<String, Value>> {
    match event {
        a2a::StreamResponse::Task(task) => task.metadata.as_ref(),
        a2a::StreamResponse::Message(msg) => msg.metadata.as_ref(),
        a2a::StreamResponse::StatusUpdate(update) => update.metadata.as_ref(),
        a2a::StreamResponse::ArtifactUpdate(update) => update.metadata.as_ref(),
    }
}

/// Serializes `data` to JSON and decodes it as a JSON object map. Serializing through
/// [`gadk_model::to_go_json`] writes float32 values in their shortest text (`23.3`, not
/// `23.299999237060547`). `Ok(None)` is a `null` value.
pub(crate) fn to_map_structure<T: Serialize + ?Sized>(
    data: &T,
) -> Result<Option<Map<String, Value>>, Error> {
    let bytes = gadk_model::to_go_json(data)?;
    match serde_json::from_slice::<Value>(&bytes)? {
        Value::Object(map) => Ok(Some(map)),
        Value::Null => Ok(None),
        other => Err(unmarshal_into_map_error(&other)),
    }
}

/// The error for a non-object JSON value decoded into a map:
/// `expected a JSON object, found <kind>` (e.g. `found an array`).
pub(crate) fn unmarshal_into_map_error(value: &Value) -> Error {
    Error::msg(gadk_platform::json_expected_found("a JSON object", value))
}

/// Stores `data` under `key` as a JSON object; absent or `null` data is skipped.
fn add_meta<T: Serialize>(
    result: &mut HashMap<String, Value>,
    key: &str,
    data: Option<&T>,
) -> Result<(), Error> {
    let Some(data) = data else {
        return Ok(());
    };
    if let Some(map) = to_map_structure(data)? {
        result.insert(key.to_owned(), Value::Object(map));
    }
    Ok(())
}

/// Decodes the JSON object under `key`, when there is one, into `target`.
fn process_meta<T: DeserializeOwned>(
    key: &str,
    meta: Option<&HashMap<String, Value>>,
    target: &mut Option<T>,
) -> Result<(), Error> {
    if let Some(Value::Object(map)) = meta.and_then(|m| m.get(key)) {
        *target = Some(from_map_structure(map)?);
    }
    Ok(())
}

/// Serializes `data` to JSON text and decodes it as `T`. Going through JSON text matters:
/// a2a-rs's JSON-RPC and REST clients decode metadata through protobuf `Struct`, so every number
/// arrives as an `f64` (`12.0`), which serde refuses for an integer field;
/// [`gadk_model::to_go_json`] writes an integral float as `12`, which decodes.
pub(crate) fn from_map_structure<T: DeserializeOwned>(
    data: &Map<String, Value>,
) -> Result<T, Error> {
    let bytes = gadk_model::to_go_json(data)?;
    Ok(serde_json::from_slice(&bytes)?)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use serde_json::json;

    /// a2a-rs clients decode metadata through protobuf `Struct`, so integers arrive as `f64`;
    /// Go's `FromMapStructure` goes through JSON text, where `12.0` is written `12`.
    #[test]
    fn integral_floats_decode_into_integer_fields() {
        let meta = HashMap::from([
            (
                METADATA_USAGE_KEY.to_owned(),
                json!({"candidatesTokenCount": 12.0, "thoughtsTokenCount": 42.0}),
            ),
            (
                METADATA_CUSTOM_META_KEY.to_owned(),
                json!({"count": 3.0, "ratio": 0.5}),
            ),
        ]);
        let mut event = Event::default();
        process_meta_view("t", "c", Some(&meta), None, &mut event).unwrap();
        let usage = event.response.usage_metadata.unwrap();
        assert_eq!(usage.candidates_token_count, 12);
        assert_eq!(usage.thoughts_token_count, 42);
        assert_eq!(event.response.custom_metadata.get("count"), Some(&json!(3)));
        assert_eq!(
            event.response.custom_metadata.get("ratio"),
            Some(&json!(0.5))
        );
    }
}
