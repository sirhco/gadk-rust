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

//! The artifact makers of the two output modes (port of ADK-Go
//! `server/adka2a/v2/task_artifact.go`).

use std::collections::HashMap;

use gadk_session::Event;
use serde_json::Value;

use super::processor::{TaskInfo, flag, new_artifact_event, new_artifact_update_event};
use crate::convert::METADATA_PARTIAL_KEY;

/// Go `eventToArtifactTransform`: turns the converted parts of one ADK event into an artifact
/// update, and makes the update that closes the run.
pub(crate) trait EventToArtifactTransform: Send + Sync {
    /// Go `transform`.
    fn transform(
        &mut self,
        event: &Event,
        parts: Vec<a2a::Part>,
        meta: &HashMap<String, Value>,
    ) -> a2a::TaskArtifactUpdateEvent;
    /// Go `makeFinalUpdate`.
    fn make_final_update(&self) -> Option<a2a::TaskArtifactUpdateEvent>;
}

/// Go `artifactMaker` ([`OutputMode::ArtifactPerEvent`](super::OutputMode)): one artifact per
/// author's chain of partial events, sealed by the next non-partial event.
pub(crate) struct ArtifactMaker {
    task: TaskInfo,
    last_agent_partial_artifact: HashMap<String, String>,
}

impl ArtifactMaker {
    /// Go `newArtifactMaker`.
    pub(crate) fn new(task: TaskInfo) -> Self {
        Self {
            task,
            last_agent_partial_artifact: HashMap::new(),
        }
    }
}

impl EventToArtifactTransform for ArtifactMaker {
    fn transform(
        &mut self,
        event: &Event,
        parts: Vec<a2a::Part>,
        meta: &HashMap<String, Value>,
    ) -> a2a::TaskArtifactUpdateEvent {
        let mut result = new_artifact_event(&self.task, parts);
        let partial = event.response.partial;

        if let Some(artifact_id) = self.last_agent_partial_artifact.get(&event.author) {
            result.artifact.artifact_id = artifact_id.clone();
            // continue accumulating if partial, otherwise replace contents
            result.append = flag(partial);
        }
        result.last_chunk = flag(!partial);

        if partial {
            self.last_agent_partial_artifact
                .insert(event.author.clone(), result.artifact.artifact_id.clone());
        } else {
            self.last_agent_partial_artifact.remove(&event.author);
        }

        if !meta.is_empty() {
            result
                .artifact
                .metadata
                .get_or_insert_with(HashMap::new)
                .extend(meta.iter().map(|(k, v)| (k.clone(), v.clone())));
        }
        result
    }

    fn make_final_update(&self) -> Option<a2a::TaskArtifactUpdateEvent> {
        None
    }
}

/// Go `legacyArtifactMaker` ([`OutputMode::ArtifactPerRun`](super::OutputMode)): one artifact for
/// the run's non-partial events, and an "ephemeral" artifact replaced by every partial event and
/// reset when the run ends.
pub(crate) struct LegacyArtifactMaker {
    task: TaskInfo,
    /// Go `responseID`: set once the first non-partial update was made.
    response_id: String,
    /// Go `partialResponseID`: set once the first partial update was made.
    partial_response_id: String,
}

impl LegacyArtifactMaker {
    /// Go `newLegacyArtifactMaker`.
    pub(crate) fn new(task: TaskInfo) -> Self {
        Self {
            task,
            response_id: String::new(),
            partial_response_id: String::new(),
        }
    }
}

impl EventToArtifactTransform for LegacyArtifactMaker {
    fn transform(
        &mut self,
        event: &Event,
        parts: Vec<a2a::Part>,
        meta: &HashMap<String, Value>,
    ) -> a2a::TaskArtifactUpdateEvent {
        let mut result = if event.response.partial {
            let result =
                new_legacy_partial_artifact_update(&self.task, &self.partial_response_id, parts);
            self.partial_response_id = result.artifact.artifact_id.clone();
            result
        } else {
            let result = new_legacy_artifact_update(&self.task, &self.response_id, parts);
            self.response_id = result.artifact.artifact_id.clone();
            result
        };
        if !meta.is_empty() {
            result
                .metadata
                .get_or_insert_with(HashMap::new)
                .extend(meta.iter().map(|(k, v)| (k.clone(), v.clone())));
        }
        result
    }

    fn make_final_update(&self) -> Option<a2a::TaskArtifactUpdateEvent> {
        // We could also send a LastChunk: true event for the main (non-partial) artifact,
        // but there's currently no special handling for it and not all A2A SDK (eg. Java)
        // implementations allow empty-part artifact updates.
        if self.partial_response_id.is_empty() {
            return None;
        }
        let mut ev = new_legacy_partial_artifact_update(
            &self.task,
            &self.partial_response_id,
            vec![a2a::Part::data(Value::Object(serde_json::Map::new()))],
        );
        ev.last_chunk = Some(true);
        Some(ev)
    }
}

/// Go `newLegacyArtifactUpdate`: a new artifact when `id` is empty, else an append to `id`,
/// explicitly marked as a non-partial ADK event so the consumer does not run its own
/// aggregation.
pub(crate) fn new_legacy_artifact_update(
    task: &TaskInfo,
    id: &str,
    parts: Vec<a2a::Part>,
) -> a2a::TaskArtifactUpdateEvent {
    let mut result = if id.is_empty() {
        new_artifact_event(task, parts)
    } else {
        new_artifact_update_event(task, id, parts)
    };
    result.metadata = Some(HashMap::from([(
        METADATA_PARTIAL_KEY.to_owned(),
        Value::Bool(false),
    )]));
    result
}

/// Go `newLegacyPartialArtifactUpdate`: replaces the partial artifact `id` (a new one when
/// empty), marking the update, the artifact and each part as ADK-partial.
pub(crate) fn new_legacy_partial_artifact_update(
    task: &TaskInfo,
    id: &str,
    mut parts: Vec<a2a::Part>,
) -> a2a::TaskArtifactUpdateEvent {
    // Go marks the parts after building the update; its parts are shared pointers, so the
    // artifact's parts carry the mark too.
    for part in &mut parts {
        part.metadata
            .get_or_insert_with(HashMap::new)
            .insert(METADATA_PARTIAL_KEY.to_owned(), Value::Bool(true));
    }
    let mut ev = new_legacy_artifact_update(task, id, parts);
    ev.artifact
        .metadata
        .get_or_insert_with(HashMap::new)
        .insert(METADATA_PARTIAL_KEY.to_owned(), Value::Bool(true));
    ev.metadata
        .get_or_insert_with(HashMap::new)
        .insert(METADATA_PARTIAL_KEY.to_owned(), Value::Bool(true));
    ev.append = None; // discard partial events
    ev
}
