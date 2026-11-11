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

//! The ADK A2A extension marker (port of ADK-Go `server/adka2a/v2/extension.go`).

use std::collections::HashMap;

use serde_json::Value;

/// Go `ADKExtensionURI`: the metadata key the executor writes on every task and task status
/// update it emits, telling a client that the payload follows ADK conventions (content spread
/// across task artifacts and the status message, long-running function calls carried by the
/// latter).
pub const ADK_EXTENSION_URI: &str = "https://google.github.io/adk-docs/a2a/a2a-extension/";

/// Go `addADKExtensionMeta`: adds [`ADK_EXTENSION_URI`] to the metadata of events whose metadata
/// reaches the task's metadata — tasks and task status updates. Messages and artifact updates
/// (whose metadata is merged into the artifact) are left alone.
// The server executor calls it; builds without `server` do not.
#[allow(dead_code)]
pub(crate) fn add_adk_extension_meta(event: &mut a2a::StreamResponse) {
    match event {
        a2a::StreamResponse::Task(task) => {
            task.metadata = Some(meta_with_adk_extension(task.metadata.take()));
        }
        a2a::StreamResponse::StatusUpdate(update) => {
            update.metadata = Some(meta_with_adk_extension(update.metadata.take()));
        }
        a2a::StreamResponse::Message(_) | a2a::StreamResponse::ArtifactUpdate(_) => {}
    }
}

/// Go `metaWithADKExtension`: `meta` (or an empty map) holding [`ADK_EXTENSION_URI`] = `true`.
/// Go copies the map so a map shared with other events never sees the key; taking the map by
/// value gives the same guarantee.
#[allow(dead_code)]
pub(crate) fn meta_with_adk_extension(
    meta: Option<HashMap<String, Value>>,
) -> HashMap<String, Value> {
    let mut result = meta.unwrap_or_default();
    result.insert(ADK_EXTENSION_URI.to_owned(), Value::Bool(true));
    result
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use serde_json::json;

    fn status_update(metadata: Option<HashMap<String, Value>>) -> a2a::StreamResponse {
        a2a::StreamResponse::StatusUpdate(a2a::TaskStatusUpdateEvent {
            task_id: "t".into(),
            context_id: "c".into(),
            status: a2a::TaskStatus {
                state: a2a::TaskState::Working,
                message: None,
                timestamp: None,
            },
            metadata,
        })
    }

    #[test]
    fn extension_key_is_added_to_tasks_and_status_updates_only() {
        let mut task = a2a::StreamResponse::Task(a2a::Task {
            id: "t".into(),
            context_id: "c".into(),
            status: a2a::TaskStatus {
                state: a2a::TaskState::Submitted,
                message: None,
                timestamp: None,
            },
            artifacts: None,
            history: None,
            metadata: None,
        });
        add_adk_extension_meta(&mut task);
        let a2a::StreamResponse::Task(task) = task else {
            unreachable!()
        };
        assert_eq!(
            serde_json::to_value(task.metadata).unwrap(),
            json!({ (ADK_EXTENSION_URI): true })
        );

        let mut update = status_update(Some(HashMap::from([("k".to_owned(), json!("v"))])));
        add_adk_extension_meta(&mut update);
        let a2a::StreamResponse::StatusUpdate(update) = update else {
            unreachable!()
        };
        assert_eq!(
            serde_json::to_value(update.metadata).unwrap(),
            json!({ "k": "v", (ADK_EXTENSION_URI): true })
        );

        let mut artifact = a2a::StreamResponse::ArtifactUpdate(a2a::TaskArtifactUpdateEvent {
            task_id: "t".into(),
            context_id: "c".into(),
            artifact: a2a::Artifact {
                artifact_id: "a".into(),
                name: None,
                description: None,
                parts: vec![],
                metadata: None,
                extensions: None,
            },
            append: None,
            last_chunk: None,
            metadata: None,
        });
        add_adk_extension_meta(&mut artifact);
        let a2a::StreamResponse::ArtifactUpdate(artifact) = artifact else {
            unreachable!()
        };
        assert_eq!(artifact.metadata, None);

        let mut message = a2a::StreamResponse::Message(a2a::Message::new(a2a::Role::Agent, vec![]));
        add_adk_extension_meta(&mut message);
        let a2a::StreamResponse::Message(message) = message else {
            unreachable!()
        };
        assert_eq!(message.metadata, None);
    }
}
