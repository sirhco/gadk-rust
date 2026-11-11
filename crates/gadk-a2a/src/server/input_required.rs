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

//! Input-required handling (port of ADK-Go `server/adka2a/v2/input_required.go`).

use std::collections::HashMap;
#[cfg(feature = "client")]
use std::collections::HashSet;

#[cfg(feature = "client")]
use gadk_engine::tool::function_calls;
use gadk_genai::{Content, Part};
use gadk_platform::go_quote;
use gadk_session::Event;
#[cfg(feature = "client")]
use gadk_session::Session;
use serde_json::Value;

use super::processor::{TaskInfo, new_message_for_task, new_status_update};
use crate::Error;
#[cfg(feature = "client")]
use crate::convert::get_a2a_task_info;
use crate::convert::{
    DATA_PART_META_TYPE_KEY, DATA_PART_TYPE_FUNCTION_CALL, GenAIPartConverter, part_meta,
    to_a2a_parts, to_genai_parts,
};

/// The part metadata key marking the missing-input error part.
const VALIDATION_ERROR_KEY: &str = "validation_error";

/// Go `inputRequiredProcessor`: collects the long-running function calls (and their interim
/// responses) of a run into the input-required status update that ends it.
pub(crate) struct InputRequiredProcessor {
    task: TaskInfo,
    event: Option<a2a::TaskStatusUpdateEvent>,
    part_converter: Option<GenAIPartConverter>,
    /// Go `addedParts`: handles possible duplication in partial and non-partial events.
    added_parts: Vec<Part>,
}

impl InputRequiredProcessor {
    /// Go `newInputRequiredProcessor`.
    pub(crate) fn new(task: TaskInfo, part_converter: Option<GenAIPartConverter>) -> Self {
        Self {
            task,
            event: None,
            part_converter,
            added_parts: Vec::new(),
        }
    }

    /// Go `inputRequiredProcessor.event`: the input-required status update, once a long-running
    /// call was seen.
    pub(crate) fn event(&self) -> Option<&a2a::TaskStatusUpdateEvent> {
        self.event.as_ref()
    }

    /// Go `process`: moves the long-running function calls and responses of `event` into the
    /// input-required status update. Returns the event without those parts when any were
    /// taken (`None` means `event` is unchanged), so they are not duplicated in the response.
    pub(crate) async fn process(&mut self, event: &Event) -> Result<Option<Event>, Error> {
        let Some(content) = &event.response.content else {
            return Ok(None);
        };

        let mut long_running_call_ids = Vec::new();
        let mut input_required_parts = Vec::new();
        let mut remaining_parts = Vec::new();
        for part in &content.parts {
            let mut call_id = "";
            if let Some(call) = &part.function_call
                && event.long_running_tool_ids.contains(&call.id)
            {
                call_id = &call.id;
            }
            if self.is_long_running_response(event, part)
                && let Some(response) = &part.function_response
            {
                call_id = &response.id;
            }
            if call_id.is_empty() {
                remaining_parts.push(part.clone());
                continue;
            }
            let added = self.added_parts.iter().any(|p| {
                if let (Some(a), Some(b)) = (&part.function_call, &p.function_call)
                    && a.id == b.id
                {
                    return true;
                }
                matches!(
                    (&part.function_response, &p.function_response),
                    (Some(a), Some(b)) if a.id == b.id
                )
            });
            if added {
                continue;
            }
            self.added_parts.push(part.clone());
            input_required_parts.push(part.clone());
            long_running_call_ids.push(call_id.to_owned());
        }

        if !input_required_parts.is_empty() {
            let a2a_parts = self
                .convert_parts(event, &input_required_parts, &long_running_call_ids)
                .await
                .map_err(|e| {
                    Error::wrap("failed to convert input required parts to A2A parts", e)
                })?;

            match self
                .event
                .as_mut()
                .and_then(|ev| ev.status.message.as_mut())
            {
                Some(msg) => msg.parts.extend(a2a_parts),
                None => {
                    let msg = a2a::Message::new(a2a::Role::Agent, a2a_parts);
                    self.event = Some(new_status_update(
                        &self.task,
                        a2a::TaskState::InputRequired,
                        Some(msg),
                    ));
                }
            }
        }

        if remaining_parts.len() == content.parts.len() {
            return Ok(None);
        }

        let mut modified = event.clone();
        modified.response.content = Some(Content {
            parts: remaining_parts,
            role: content.role.clone(),
        });
        Ok(Some(modified))
    }

    /// Go `convertParts`.
    async fn convert_parts(
        &self,
        event: &Event,
        parts: &[Part],
        long_running_call_ids: &[String],
    ) -> Result<Vec<a2a::Part>, Error> {
        let Some(converter) = &self.part_converter else {
            return to_a2a_parts(parts, long_running_call_ids);
        };
        let mut converted = Vec::with_capacity(parts.len());
        for part in parts {
            if let Some(part) = converter(Some(event), part).await? {
                converted.push(part);
            }
        }
        Ok(converted)
    }

    /// Go `isLongRunningResponse`: a function response to a long-running call of this event or
    /// of a call already in the input-required status message.
    fn is_long_running_response(&self, event: &Event, part: &Part) -> bool {
        let Some(response) = &part.function_response else {
            return false;
        };
        let id = &response.id;
        if event.long_running_tool_ids.contains(id) {
            return true;
        }
        let Some(msg) = self
            .event
            .as_ref()
            .and_then(|ev| ev.status.message.as_ref())
        else {
            return false;
        };
        msg.parts.iter().any(|part| {
            let a2a::PartContent::Data(Value::Object(data)) = &part.content else {
                return false;
            };
            matches!(
                part_meta(part, DATA_PART_META_TYPE_KEY),
                Some(Value::String(t)) if t == DATA_PART_TYPE_FUNCTION_CALL
            ) && matches!(data.get("id"), Some(Value::String(call_id)) if call_id == id)
        })
    }
}

/// Go `HandleInputRequired`: checks whether the input message answers every function call the
/// previous invocation recorded in the task's input-required status message. When it does not,
/// the returned event (still input-required, naming the first unanswered call) is the result of
/// the execution.
pub async fn handle_input_required(
    ctx: &a2a_server::ExecutorContext,
    content: &Content,
) -> Result<Option<a2a::TaskStatusUpdateEvent>, Error> {
    let Some(task) = &ctx.stored_task else {
        return Ok(None);
    };
    let Some(status_msg) = &task.status.message else {
        return Ok(None);
    };
    if task.status.state != a2a::TaskState::InputRequired {
        return Ok(None);
    }

    let task_parts = to_genai_parts(&status_msg.parts)
        .map_err(|e| Error::wrap("failed to parse task status message", e))?;

    for status_part in &task_parts {
        let Some(call) = &status_part.function_call else {
            continue;
        };
        let has_matching_response = content
            .parts
            .iter()
            .any(|p| matches!(&p.function_response, Some(r) if r.id == call.id));
        if !has_matching_response {
            let parts = make_input_missing_error_message(&status_msg.parts, &call.id);
            let msg = new_message_for_task(a2a::Role::Agent, &TaskInfo::of_task(task), parts);
            return Ok(Some(new_status_update(
                &TaskInfo::of(ctx),
                a2a::TaskState::InputRequired,
                Some(msg),
            )));
        }
    }
    Ok(None)
}

/// Go `subagentTask`: a remote sub-agent's task waiting for input.
// Used by the cleanup's child-task cancel, which needs the client side (plan D12).
#[cfg(feature = "client")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SubagentTask {
    pub(crate) agent_name: String,
    pub(crate) task_id: String,
}

/// Go `getSubagentTasksToCancel`: the remote tasks behind the pending long-running calls of
/// `status`, found from the newest session event backwards.
// Used by the cleanup's child-task cancel, which needs the client side (plan D12).
#[cfg(feature = "client")]
pub(crate) fn get_subagent_tasks_to_cancel(
    status: &a2a::TaskStatus,
    session: &dyn Session,
) -> Result<Vec<SubagentTask>, Error> {
    let pending_call_ids = get_pending_long_running_call_ids(status)
        .map_err(|e| Error::wrap("malformed task status", e))?;
    if pending_call_ids.is_empty() {
        return Ok(Vec::new());
    }

    let mut found_calls: HashSet<String> = HashSet::new();
    let mut found_task_ids = 0usize;
    let mut tasks_to_cancel = Vec::new();
    let events = session.events();
    for event in events.iter().rev() {
        for call in function_calls(event.response.content.as_ref()) {
            if !pending_call_ids.contains(&call.id) {
                continue;
            }
            if found_calls.contains(&call.id) {
                tracing::warn!(id = %call.id, "duplicate function call id");
                continue;
            }
            found_calls.insert(call.id.clone());
            let (task_id, _) = get_a2a_task_info(event);
            if !task_id.is_empty() {
                tasks_to_cancel.push(SubagentTask {
                    agent_name: event.author.clone(),
                    task_id,
                });
                found_task_ids += 1;
            }
        }
        if found_calls.len() == pending_call_ids.len() {
            break;
        }
    }

    if found_calls.len() < pending_call_ids.len() {
        tracing::warn!(
            found = found_calls.len(),
            total = pending_call_ids.len(),
            "could not find all function calls from status message"
        );
    }

    if found_task_ids < found_calls.len() {
        tracing::warn!(
            found_calls = found_calls.len(),
            found_ids = found_task_ids,
            "not all function calls had a taskID"
        );
    }

    Ok(tasks_to_cancel)
}

/// Go `getPendingLongRunningCallIDs`: the function-call IDs in an input-required status
/// message (none for any other state).
// Used by the cleanup's child-task cancel, which needs the client side (plan D12).
#[cfg(feature = "client")]
pub(crate) fn get_pending_long_running_call_ids(
    status: &a2a::TaskStatus,
) -> Result<Vec<String>, Error> {
    let Some(status_msg) = &status.message else {
        return Ok(Vec::new());
    };
    if status.state != a2a::TaskState::InputRequired {
        return Ok(Vec::new());
    }
    let task_parts = to_genai_parts(&status_msg.parts)
        .map_err(|e| Error::wrap("failed to parse task status message", e))?;
    Ok(task_parts
        .into_iter()
        .filter_map(|p| p.function_call.map(|call| call.id))
        .collect())
}

/// Go `makeInputMissingErrorMessage`: the input-required parts without earlier validation
/// errors, plus a validation error naming `call_id`.
fn make_input_missing_error_message(
    input_required_parts: &[a2a::Part],
    call_id: &str,
) -> Vec<a2a::Part> {
    let mut err_part = a2a::Part::text(format!(
        "no input provided for function call ID {}",
        go_quote(call_id)
    ));
    err_part.metadata = Some(HashMap::from([(
        VALIDATION_ERROR_KEY.to_owned(),
        Value::Bool(true),
    )]));

    let mut preserved: Vec<a2a::Part> = input_required_parts
        .iter()
        .filter(|p| !matches!(part_meta(p, VALIDATION_ERROR_KEY), Some(Value::Bool(true))))
        .cloned()
        .collect();
    preserved.push(err_part);
    preserved
}
