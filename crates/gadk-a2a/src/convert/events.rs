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
//! `WithoutPartialArtifacts`). Bodies: plan 5b Task 3.

#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use gadk_agent::InvocationContext;
use gadk_session::Event;
use serde_json::Value;

use super::A2APartConverter;
use crate::Error;

/// Go `NewRemoteAgentEvent`.
pub fn new_remote_agent_event(ctx: &InvocationContext) -> Event {
    todo!("plan 5b Task 3")
}

/// Go `EventToMessage`.
pub fn event_to_message(event: &Event) -> Result<Option<a2a::Message>, Error> {
    todo!("plan 5b Task 3")
}

/// Go `ToSessionEvent`.
pub async fn to_session_event(
    ctx: &InvocationContext,
    event: &a2a::StreamResponse,
) -> Result<Option<Event>, Error> {
    todo!("plan 5b Task 3")
}

/// Go `ToSessionEventWithParts`.
pub async fn to_session_event_with_parts(
    ctx: &InvocationContext,
    event: &a2a::StreamResponse,
    converter: Option<&A2APartConverter>,
) -> Result<Option<Event>, Error> {
    todo!("plan 5b Task 3")
}

/// Go `ToCustomMetadata`.
pub fn to_custom_metadata(task_id: &str, context_id: &str) -> serde_json::Map<String, Value> {
    todo!("plan 5b Task 3")
}

/// Go `GetA2ATaskInfo`: `(task_id, context_id)`.
pub fn get_a2a_task_info(event: &Event) -> (String, String) {
    todo!("plan 5b Task 3")
}

/// Go `WithoutPartialArtifacts`.
pub fn without_partial_artifacts(artifacts: Vec<a2a::Artifact>) -> Vec<a2a::Artifact> {
    todo!("plan 5b Task 3")
}
