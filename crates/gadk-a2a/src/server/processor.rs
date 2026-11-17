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

//! The event processor and failed-status helpers, plus the A2A event constructors the executor
//! uses (status updates, task messages, artifact events).

use std::collections::HashMap;

use gadk_model::LlmResponse;
use gadk_platform::go_quote;
use gadk_session::{Event, EventActions};
use serde_json::Value;

use super::context::InvocationMeta;
use super::input_required::InputRequiredProcessor;
use super::task_artifact::EventToArtifactTransform;
use crate::Error;
use crate::convert::{
    GenAIPartConverter, METADATA_IS_ERR_MESSAGE_KEY, set_actions_meta, to_a2a_parts, to_event_meta,
};

/// The task and context an event belongs to.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct TaskInfo {
    pub(crate) task_id: String,
    pub(crate) context_id: String,
}

impl TaskInfo {
    /// The task and context of an executor context.
    pub(crate) fn of(ctx: &a2a_server::ExecutorContext) -> Self {
        Self {
            task_id: ctx.task_id.clone(),
            context_id: ctx.context_id.clone(),
        }
    }

    /// The task and context of a task.
    pub(crate) fn of_task(task: &a2a::Task) -> Self {
        Self {
            task_id: task.id.clone(),
            context_id: task.context_id.clone(),
        }
    }
}

/// `Some(true)` for `true`, else absent.
pub(crate) fn flag(value: bool) -> Option<bool> {
    value.then_some(true)
}

/// A status update stamped with the current time.
pub(crate) fn new_status_update(
    task: &TaskInfo,
    state: a2a::TaskState,
    message: Option<a2a::Message>,
) -> a2a::TaskStatusUpdateEvent {
    a2a::TaskStatusUpdateEvent {
        task_id: task.task_id.clone(),
        context_id: task.context_id.clone(),
        status: a2a::TaskStatus {
            state,
            message,
            timestamp: Some(chrono::Utc::now()),
        },
        metadata: None,
    }
}

/// A message of `role` addressed to `task`.
pub(crate) fn new_message_for_task(
    role: a2a::Role,
    task: &TaskInfo,
    parts: Vec<a2a::Part>,
) -> a2a::Message {
    let mut msg = a2a::Message::new(role, parts);
    msg.task_id = Some(task.task_id.clone());
    msg.context_id = Some(task.context_id.clone());
    msg
}

/// A new artifact (fresh ID) holding `parts`.
pub(crate) fn new_artifact_event(
    task: &TaskInfo,
    parts: Vec<a2a::Part>,
) -> a2a::TaskArtifactUpdateEvent {
    a2a::TaskArtifactUpdateEvent {
        task_id: task.task_id.clone(),
        context_id: task.context_id.clone(),
        artifact: a2a::Artifact {
            artifact_id: a2a::new_artifact_id(),
            name: None,
            description: None,
            parts,
            metadata: None,
            extensions: None,
        },
        append: None,
        last_chunk: None,
        metadata: None,
    }
}

/// Appends `parts` to the artifact `id`.
pub(crate) fn new_artifact_update_event(
    task: &TaskInfo,
    id: &str,
    parts: Vec<a2a::Part>,
) -> a2a::TaskArtifactUpdateEvent {
    let mut ev = new_artifact_event(task, parts);
    ev.artifact.artifact_id = id.to_owned();
    ev.append = Some(true);
    ev
}

/// `meta`, or `None` when it is empty (an empty map is left off the wire).
pub(crate) fn non_empty(meta: HashMap<String, Value>) -> Option<HashMap<String, Value>> {
    (!meta.is_empty()).then_some(meta)
}

/// Converts the runner's ADK events to artifact updates and tracks what
/// the final status update will be.
pub(crate) struct EventProcessor {
    task: TaskInfo,
    pub(crate) meta: InvocationMeta,
    part_converter: Option<GenAIPartConverter>,
    /// The escalate and agent-transfer actions of the processed events,
    /// passed to the caller in the terminal event's metadata (intermediate events without parts
    /// might be ignored).
    terminal_actions: EventActions,
    /// The final status update once an LLM error response was seen; sent after
    /// the whole response was saved as an artifact.
    failed_event: Option<a2a::TaskStatusUpdateEvent>,
    /// Its event is the final status update when there is no
    /// failure.
    input_required: InputRequiredProcessor,
    /// Turns the converted parts of one event into an artifact update.
    event_to_artifact: Box<dyn EventToArtifactTransform>,
}

impl EventProcessor {
    /// A processor for `task` that builds artifacts with `transform`.
    pub(crate) fn new(
        task: TaskInfo,
        meta: InvocationMeta,
        converter: Option<GenAIPartConverter>,
        transform: Box<dyn EventToArtifactTransform>,
    ) -> Self {
        Self {
            input_required: InputRequiredProcessor::new(task.clone(), converter.clone()),
            task,
            meta,
            part_converter: converter,
            terminal_actions: EventActions::default(),
            failed_event: None,
            event_to_artifact: transform,
        }
    }

    /// The artifact update for `event`, if it has parts left after the
    /// input-required processing.
    pub(crate) async fn process(
        &mut self,
        event: &Event,
    ) -> Result<Option<a2a::TaskArtifactUpdateEvent>, Error> {
        self.update_terminal_actions(event);

        let event_meta = to_event_meta(&self.meta.event_meta, event)?;

        let modified = self
            .input_required
            .process(event)
            .await
            .map_err(|e| Error::wrap("input required processing failed", e))?;
        let event = modified.as_ref().unwrap_or(event);

        let parts = self.convert_parts(event).await?;

        let resp = &event.response;
        if (!resp.error_code.is_empty() || !resp.error_message.is_empty())
            && self.failed_event.is_none()
        {
            // terminal event might add additional keys to its metadata when it's dispatched and
            // these changes should not be reflected in this event's metadata
            let mut failed = to_task_failed_update_event(
                &self.task,
                &error_from_response(resp),
                non_empty(event_meta.clone()),
            );
            if let Some(msg) = failed.status.message.as_mut() {
                msg.parts.extend(parts.iter().cloned());
            }
            self.failed_event = Some(failed);
        }

        if parts.is_empty() {
            return Ok(None);
        }
        Ok(Some(self.event_to_artifact.transform(
            event,
            parts,
            &event_meta,
        )))
    }

    /// The update that resets the partial artifact, if one was made.
    pub(crate) fn make_final_artifact_update(&self) -> Option<a2a::TaskArtifactUpdateEvent> {
        self.event_to_artifact.make_final_update()
    }

    /// The failure, else the input-required update, else
    /// `completed`, carrying the terminal actions.
    pub(crate) fn make_final_status_update(&self) -> a2a::TaskStatusUpdateEvent {
        if let Some(event) = self.failed_event.as_ref().or(self.input_required.event()) {
            let mut event = event.clone();
            event.metadata = with_actions(event.metadata.take(), &self.terminal_actions);
            return event;
        }

        let mut ev = new_status_update(&self.task, a2a::TaskState::Completed, None);
        // we're modifying base processor metadata which might have been sent with one of the
        // previous events. this update shouldn't be reflected in the sent events' metadata.
        ev.metadata = with_actions(Some(self.meta.event_meta.clone()), &self.terminal_actions);
        ev
    }

    /// A failed status update for `cause`, with the metadata of
    /// `event` when it converts.
    pub(crate) fn make_task_failed_event(
        &self,
        cause: &str,
        event: Option<&Event>,
    ) -> a2a::TaskStatusUpdateEvent {
        let mut meta = self.meta.event_meta.clone();
        if let Some(event) = event {
            match to_event_meta(&self.meta.event_meta, event) {
                Ok(event_meta) => meta = event_meta,
                Err(err) => tracing::warn!(
                    cause = %err,
                    "failed to convert event metadata for task failed event"
                ),
            }
        }
        to_task_failed_update_event(&self.task, cause, non_empty(meta))
    }

    /// Folds the escalate and transfer-to-agent actions of `event` into the terminal actions.
    fn update_terminal_actions(&mut self, event: &Event) {
        self.terminal_actions.escalate = self.terminal_actions.escalate || event.actions.escalate;
        if !event.actions.transfer_to_agent.is_empty() {
            self.terminal_actions.transfer_to_agent = event.actions.transfer_to_agent.clone();
        }
    }

    /// Converts the parts of `event` to A2A parts, through the part converter when one is set.
    async fn convert_parts(&self, event: &Event) -> Result<Vec<a2a::Part>, Error> {
        let parts = match &event.response.content {
            Some(content) if !content.parts.is_empty() => &content.parts,
            _ => return Ok(Vec::new()),
        };
        let Some(converter) = &self.part_converter else {
            return to_a2a_parts(parts, &event.long_running_tool_ids);
        };
        let mut converted = Vec::with_capacity(parts.len());
        for part in parts {
            if let Some(part) = converter(Some(event), part).await? {
                converted.push(part);
            }
        }
        Ok(converted)
    }
}

/// `meta` with the actions written to it; an empty result is `None`.
fn with_actions(
    meta: Option<HashMap<String, Value>>,
    actions: &EventActions,
) -> Option<HashMap<String, Value>> {
    let mut meta = meta.unwrap_or_default();
    set_actions_meta(&mut meta, actions);
    non_empty(meta)
}

/// A failed status update whose message is `cause`, marked as an
/// error message.
pub(crate) fn to_task_failed_update_event(
    task: &TaskInfo,
    cause: &str,
    meta: Option<HashMap<String, Value>>,
) -> a2a::TaskStatusUpdateEvent {
    let mut part = a2a::Part::text(cause);
    part.metadata = Some(HashMap::from([(
        METADATA_IS_ERR_MESSAGE_KEY.to_owned(),
        Value::Bool(true),
    )]));
    let msg = new_message_for_task(a2a::Role::Agent, task, vec![part]);
    let mut ev = new_status_update(task, a2a::TaskState::Failed, Some(msg));
    ev.metadata = meta;
    ev
}

/// The error text of an LLM error response: `llm error response (code <code>): <quoted message>`,
/// without the code part when there is none.
pub(crate) fn error_from_response(resp: &LlmResponse) -> String {
    if resp.error_code.is_empty() {
        return format!("llm error response: {}", go_quote(&resp.error_message));
    }
    format!(
        "llm error response (code {}): {}",
        resp.error_code,
        go_quote(&resp.error_message)
    )
}
