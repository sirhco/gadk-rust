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

//! The event types written to the `event_type` column and `newSet`, from ADK-Go
//! `plugin/agentanalytics/bigquery_agent_analytics_plugin.go` (the rest of that file is 7b-3).
//! The names match adk-python, so rows from either language can be queried together.

use std::collections::BTreeSet;

/// A user message reached the runner.
pub const EVENT_USER_MESSAGE_RECEIVED: &str = "USER_MESSAGE_RECEIVED";
/// An invocation started.
pub const EVENT_INVOCATION_STARTING: &str = "INVOCATION_STARTING";
/// An invocation completed.
pub const EVENT_INVOCATION_COMPLETED: &str = "INVOCATION_COMPLETED";
/// An invocation failed.
pub const EVENT_INVOCATION_ERROR: &str = "INVOCATION_ERROR";
/// An agent run started.
pub const EVENT_AGENT_STARTING: &str = "AGENT_STARTING";
/// An agent run completed.
pub const EVENT_AGENT_COMPLETED: &str = "AGENT_COMPLETED";
/// An agent run failed.
pub const EVENT_AGENT_ERROR: &str = "AGENT_ERROR";
/// A model request.
pub const EVENT_LLM_REQUEST: &str = "LLM_REQUEST";
/// A model response: written once per model call, on the final non-partial response. Streamed
/// chunks with `partial` set only feed `latency_ms.time_to_first_token_ms` on that final row,
/// and only the first chunk with generated content does.
pub const EVENT_LLM_RESPONSE: &str = "LLM_RESPONSE";
/// A model call failed.
pub const EVENT_LLM_ERROR: &str = "LLM_ERROR";
/// A tool call started.
pub const EVENT_TOOL_STARTING: &str = "TOOL_STARTING";
/// A tool call completed.
pub const EVENT_TOOL_COMPLETED: &str = "TOOL_COMPLETED";
/// A tool call failed.
pub const EVENT_TOOL_ERROR: &str = "TOOL_ERROR";
/// A session event carried a state delta.
pub const EVENT_STATE_DELTA: &str = "STATE_DELTA";
/// A session event transferred to another agent.
pub const EVENT_AGENT_TRANSFER: &str = "AGENT_TRANSFER";
/// Emitted when `session.EventActions.Compaction` is populated (or when the event's custom
/// metadata carries `"adk_compaction"` on ADK versions before typed compaction).
pub const EVENT_EVENT_COMPACTION: &str = "EVENT_COMPACTION";
/// Emitted when the event's custom metadata carries `"adk_agent_state"` or `"agent_state"` (or a
/// terminal `"end_of_agent"` / `"adk_end_of_agent"` == true), mirroring adk-python's checkpoint
/// rows.
pub const EVENT_AGENT_STATE_CHECKPOINT: &str = "AGENT_STATE_CHECKPOINT";
/// A long-running tool paused.
pub const EVENT_TOOL_PAUSED: &str = "TOOL_PAUSED";
/// A human-in-the-loop credential request.
pub const EVENT_HITL_CREDENTIAL_REQUEST: &str = "HITL_CREDENTIAL_REQUEST";
/// A human-in-the-loop confirmation request.
pub const EVENT_HITL_CONFIRMATION_REQUEST: &str = "HITL_CONFIRMATION_REQUEST";
/// A human-in-the-loop input request.
pub const EVENT_HITL_INPUT_REQUEST: &str = "HITL_INPUT_REQUEST";
/// A human-in-the-loop credential request completed (no view).
pub const EVENT_HITL_CREDENTIAL_COMPLETED: &str = "HITL_CREDENTIAL_REQUEST_COMPLETED";
/// A human-in-the-loop confirmation request completed (no view).
pub const EVENT_HITL_CONFIRMATION_COMPLETED: &str = "HITL_CONFIRMATION_REQUEST_COMPLETED";
/// A human-in-the-loop input request completed (no view).
pub const EVENT_HITL_INPUT_COMPLETED: &str = "HITL_INPUT_REQUEST_COMPLETED";
/// An agent-to-agent interaction.
pub const EVENT_A2A_INTERACTION: &str = "A2A_INTERACTION";
/// An agent response.
pub const EVENT_AGENT_RESPONSE: &str = "AGENT_RESPONSE";
/// A workflow node output.
pub const EVENT_NODE_OUTPUT: &str = "NODE_OUTPUT";
/// A workflow node error.
pub const EVENT_NODE_ERROR: &str = "NODE_ERROR";

/// Go `newSet(items...)`: the items as a set (Go `map[string]struct{}`).
pub(crate) fn new_set<S: AsRef<str>>(items: &[S]) -> BTreeSet<String> {
    items.iter().map(|s| s.as_ref().to_owned()).collect()
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn event_type_names_match_go() {
        let all = [
            (EVENT_USER_MESSAGE_RECEIVED, "USER_MESSAGE_RECEIVED"),
            (EVENT_INVOCATION_STARTING, "INVOCATION_STARTING"),
            (EVENT_INVOCATION_COMPLETED, "INVOCATION_COMPLETED"),
            (EVENT_INVOCATION_ERROR, "INVOCATION_ERROR"),
            (EVENT_AGENT_STARTING, "AGENT_STARTING"),
            (EVENT_AGENT_COMPLETED, "AGENT_COMPLETED"),
            (EVENT_AGENT_ERROR, "AGENT_ERROR"),
            (EVENT_LLM_REQUEST, "LLM_REQUEST"),
            (EVENT_LLM_RESPONSE, "LLM_RESPONSE"),
            (EVENT_LLM_ERROR, "LLM_ERROR"),
            (EVENT_TOOL_STARTING, "TOOL_STARTING"),
            (EVENT_TOOL_COMPLETED, "TOOL_COMPLETED"),
            (EVENT_TOOL_ERROR, "TOOL_ERROR"),
            (EVENT_STATE_DELTA, "STATE_DELTA"),
            (EVENT_AGENT_TRANSFER, "AGENT_TRANSFER"),
            (EVENT_EVENT_COMPACTION, "EVENT_COMPACTION"),
            (EVENT_AGENT_STATE_CHECKPOINT, "AGENT_STATE_CHECKPOINT"),
            (EVENT_TOOL_PAUSED, "TOOL_PAUSED"),
            (EVENT_HITL_CREDENTIAL_REQUEST, "HITL_CREDENTIAL_REQUEST"),
            (EVENT_HITL_CONFIRMATION_REQUEST, "HITL_CONFIRMATION_REQUEST"),
            (EVENT_HITL_INPUT_REQUEST, "HITL_INPUT_REQUEST"),
            (
                EVENT_HITL_CREDENTIAL_COMPLETED,
                "HITL_CREDENTIAL_REQUEST_COMPLETED",
            ),
            (
                EVENT_HITL_CONFIRMATION_COMPLETED,
                "HITL_CONFIRMATION_REQUEST_COMPLETED",
            ),
            (EVENT_HITL_INPUT_COMPLETED, "HITL_INPUT_REQUEST_COMPLETED"),
            (EVENT_A2A_INTERACTION, "A2A_INTERACTION"),
            (EVENT_AGENT_RESPONSE, "AGENT_RESPONSE"),
            (EVENT_NODE_OUTPUT, "NODE_OUTPUT"),
            (EVENT_NODE_ERROR, "NODE_ERROR"),
        ];
        assert_eq!(all.len(), 28);
        for (got, want) in all {
            assert_eq!(got, want);
        }
        let distinct = new_set(&all.map(|(name, _)| name));
        assert_eq!(distinct.len(), 28);
    }

    #[test]
    fn new_set_dedupes() {
        let set = new_set(&["content", "attributes", "content"]);
        assert_eq!(
            set.into_iter().collect::<Vec<_>>(),
            vec!["attributes".to_owned(), "content".to_owned()]
        );
        assert!(new_set::<&str>(&[]).is_empty());
    }
}
