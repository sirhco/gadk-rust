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

//! The `Event` family: the events of a session and their JSON encoding: the same keys and
//! omission rules as ADK, RFC 3339 timestamps, numeric (adk-python) timestamps and
//! case-insensitive (legacy) key matching on decode.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use gadk_genai::Content;
use gadk_genai::serde_util::is_zero;
use gadk_model::LlmResponse;
use gadk_platform::Platform;
use serde::de::Error as _;
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};

use crate::legacy::normalize_keys;
use crate::tool_confirmation::ToolConfirmation;

/// The zero time, `0001-01-01T00:00:00Z`.
pub fn zero_time() -> DateTime<Utc> {
    DateTime::from_timestamp(ZERO_TIME_SECS, 0).unwrap_or(DateTime::<Utc>::MIN_UTC)
}

/// Unix seconds of `0001-01-01T00:00:00Z`.
const ZERO_TIME_SECS: i64 = -62_135_596_800;
/// Unix seconds of `9999-12-31T23:59:59Z`.
const MAX_TIME_SECS: i64 = 253_402_300_799;

/// Encodes and decodes a `DateTime<Utc>` as RFC 3339 with trailing-zero-trimmed nanoseconds;
/// `null` decodes to [`zero_time`].
pub(crate) mod go_time {
    use chrono::{DateTime, Datelike, Utc};
    use serde::{Deserialize, Deserializer, Serializer};

    /// Formats `t` as RFC 3339 with trailing-zero-trimmed nanoseconds; `None` outside years
    /// 0..=9999, which cannot be marshalled.
    pub(crate) fn format(t: &DateTime<Utc>) -> Option<String> {
        if !(0..=9999).contains(&t.year()) {
            return None;
        }
        Some(gadk_platform::go_format_rfc3339_nano(t))
    }

    pub(crate) fn serialize<S: Serializer>(t: &DateTime<Utc>, s: S) -> Result<S::Ok, S::Error> {
        match format(t) {
            Some(v) => s.serialize_str(&v),
            None => Err(serde::ser::Error::custom(
                "timestamp year outside of range [0,9999]",
            )),
        }
    }

    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<DateTime<Utc>, D::Error> {
        match Option::<String>::deserialize(d)? {
            None => Ok(super::zero_time()),
            Some(s) => DateTime::parse_from_rfc3339(&s)
                .map(|t| t.with_timezone(&Utc))
                .map_err(serde::de::Error::custom),
        }
    }
}

/// An interaction in a conversation between agents and users.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Event {
    /// The model response this event carries, flattened into the event's JSON.
    #[serde(flatten)]
    pub response: LlmResponse,
    /// Set by storage.
    pub id: String,
    /// Set by storage; the zero time when unset.
    #[serde(serialize_with = "go_time::serialize")]
    pub timestamp: DateTime<Utc>,
    /// Set by the agent context implementation.
    #[serde(rename = "invocationId")]
    pub invocation_id: String,
    /// The branch of the event, like `agent_1.agent_2.agent_3`.
    #[serde(skip_serializing_if = "is_zero")]
    pub branch: String,
    /// When set, the event is visible only to agent contexts with this exact isolation scope.
    #[serde(rename = "isolationScope", skip_serializing_if = "is_zero")]
    pub isolation_scope: String,
    /// The name of the event's author.
    pub author: String,
    /// The actions taken by the agent.
    pub actions: EventActions,
    /// IDs of the long-running function calls.
    #[serde(rename = "longRunningToolIds", skip_serializing_if = "Vec::is_empty")]
    pub long_running_tool_ids: Vec<String>,
    /// Routing information for workflow execution.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub routes: Vec<String>,
    /// When set, the emitting workflow node asks for human input and is about to pause.
    #[serde(rename = "requestedInput", skip_serializing_if = "Option::is_none")]
    pub requested_input: Option<RequestInput>,
    /// Generic data output from a workflow node.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<Value>,
    /// Workflow-node metadata; `None` for non-workflow events.
    #[serde(rename = "nodeInfo", skip_serializing_if = "Option::is_none")]
    pub node_info: Option<NodeInfo>,
}

impl Default for Event {
    fn default() -> Self {
        Self {
            response: LlmResponse::default(),
            id: String::new(),
            timestamp: zero_time(),
            invocation_id: String::new(),
            branch: String::new(),
            isolation_scope: String::new(),
            author: String::new(),
            actions: EventActions::default(),
            long_running_tool_ids: Vec::new(),
            routes: Vec::new(),
            requested_input: None,
            output: None,
            node_info: None,
        }
    }
}

/// JSON names of `Event` and of its flattened `LlmResponse`, for case-insensitive decoding.
const EVENT_KEYS: &[&str] = &[
    "id",
    "timestamp",
    "invocationId",
    "branch",
    "isolationScope",
    "author",
    "actions",
    "longRunningToolIds",
    "routes",
    "requestedInput",
    "output",
    "nodeInfo",
    "content",
    "citationMetadata",
    "groundingMetadata",
    "usageMetadata",
    "customMetadata",
    "logprobsResult",
    "inputTranscription",
    "outputTranscription",
    "modelVersion",
    "partial",
    "turnComplete",
    "interrupted",
    "sessionResumptionHandle",
    "errorCode",
    "errorMessage",
    "finishReason",
    "avgLogprobs",
];

/// Bounds on a decoded numeric event timestamp, in microseconds: years 1 and 9999.
const MIN_TIMESTAMP_MICROS: f64 = ZERO_TIME_SECS as f64 * 1e6;
const MAX_TIMESTAMP_MICROS: f64 = MAX_TIME_SECS as f64 * 1e6;

/// Decodes an event timestamp: an RFC 3339 string, a JSON number of epoch seconds
/// (adk-python) resolved to microseconds, or `null`/absent for the zero time.
fn decode_timestamp(raw: Option<Value>) -> Result<DateTime<Utc>, String> {
    let raw = match raw {
        None | Some(Value::Null) => return Ok(zero_time()),
        Some(v) => v,
    };
    match &raw {
        Value::String(s) => DateTime::parse_from_rfc3339(s)
            .map(|t| t.with_timezone(&Utc))
            .map_err(|e| format!("session: decoding event timestamp {raw}: {e}")),
        Value::Number(n) => {
            let secs = n
                .as_f64()
                .ok_or_else(|| format!("session: decoding event timestamp {raw}: not a number"))?;
            let micros = (secs * 1e6).round();
            let implausible = || format!("session: event timestamp {raw} is not a plausible date");
            if micros.is_nan() || !(MIN_TIMESTAMP_MICROS..=MAX_TIMESTAMP_MICROS).contains(&micros) {
                return Err(implausible());
            }
            // In range by the check above, so the conversion is exact.
            DateTime::from_timestamp_micros(micros as i64).ok_or_else(implausible)
        }
        _ => Err(format!(
            "session: decoding event timestamp {raw}: not a string or number"
        )),
    }
}

/// A `null` member decodes as if the key were absent, so a fresh value keeps its default;
/// dropping null members gives that for every field.
fn drop_nulls(map: &mut Map<String, Value>) {
    map.retain(|_, v| !v.is_null());
}

/// `Event` minus `timestamp`, decoded after key normalization.
#[derive(Default, Deserialize)]
#[serde(default)]
struct EventWire {
    #[serde(flatten)]
    response: LlmResponse,
    id: String,
    #[serde(rename = "invocationId")]
    invocation_id: String,
    branch: String,
    #[serde(rename = "isolationScope")]
    isolation_scope: String,
    author: String,
    actions: EventActions,
    #[serde(rename = "longRunningToolIds")]
    long_running_tool_ids: Vec<String>,
    routes: Vec<String>,
    #[serde(rename = "requestedInput")]
    requested_input: Option<RequestInput>,
    output: Option<Value>,
    #[serde(rename = "nodeInfo")]
    node_info: Option<NodeInfo>,
}

impl<'de> Deserialize<'de> for Event {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let mut map = Map::<String, Value>::deserialize(d)?;
        normalize_keys(&mut map, EVENT_KEYS);
        let timestamp = decode_timestamp(map.remove("timestamp")).map_err(D::Error::custom)?;
        drop_nulls(&mut map);
        let w: EventWire = serde_json::from_value(Value::Object(map)).map_err(D::Error::custom)?;
        // adk-python writes a nodeInfo object on every event; an all-zero NodeInfo carries no
        // information, so it is indistinguishable from absent.
        let node_info = w.node_info.filter(|ni| *ni != NodeInfo::default());
        Ok(Event {
            response: w.response,
            id: w.id,
            timestamp,
            invocation_id: w.invocation_id,
            branch: w.branch,
            isolation_scope: w.isolation_scope,
            author: w.author,
            actions: w.actions,
            long_running_tool_ids: w.long_running_tool_ids,
            routes: w.routes,
            requested_input: w.requested_input,
            output: w.output,
            node_info,
        })
    }
}

impl Event {
    /// Creates an event with an ID and timestamp from `platform` and both action delta maps
    /// allocated.
    ///
    /// ```
    /// use gadk_platform::Platform;
    /// let p = Platform::deterministic(1);
    /// let a = gadk_session::Event::new(&p, "inv");
    /// let b = gadk_session::Event::new(&Platform::deterministic(1), "inv");
    /// assert_eq!((a.id.clone(), a.timestamp), (b.id.clone(), b.timestamp));
    /// assert_eq!(a.actions.state_delta, Some(Default::default()));
    /// ```
    pub fn new(platform: &Platform, invocation_id: impl Into<String>) -> Self {
        Self {
            id: platform.new_uuid(),
            invocation_id: invocation_id.into(),
            timestamp: platform.now(),
            actions: EventActions {
                state_delta: Some(Map::new()),
                artifact_delta: Some(BTreeMap::new()),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    /// Whether the event is the final response of an agent.
    ///
    /// When multiple agents participate in one invocation, each may produce a final response.
    pub fn is_final_response(&self) -> bool {
        // A compaction event is bookkeeping, not conversation.
        if self.actions.compaction.is_some() {
            return false;
        }
        if self.actions.skip_summarization || !self.long_running_tool_ids.is_empty() {
            return true;
        }
        let parts = self
            .response
            .content
            .as_ref()
            .map(|c| c.parts.as_slice())
            .unwrap_or_default();
        let has_function_calls = parts.iter().any(|p| p.function_call.is_some());
        let has_function_responses = parts.iter().any(|p| p.function_response.is_some());
        // A complete error event ends the turn even when it carries a function response or a
        // code execution result; only a function call keeps it open.
        if !self.response.error_code.is_empty() && !self.response.partial && !has_function_calls {
            return true;
        }
        let has_trailing_code_execution_result = parts
            .last()
            .is_some_and(|p| p.code_execution_result.is_some());
        !has_function_calls
            && !has_function_responses
            && !self.response.partial
            && !has_trailing_code_execution_result
    }
}

/// The actions attached to an event.
///
/// `state_delta` and `artifact_delta` keep the `None`/empty distinction on the wire: `None`
/// omits the key and `Some` writes it, even when empty; `null` is never written.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EventActions {
    /// State changes, set by the agent context implementation.
    pub state_delta: Option<Map<String, Value>>,
    /// Artifact updates: filename to version.
    pub artifact_delta: Option<BTreeMap<String, i64>>,
    /// Tool confirmations requested, keyed by function call ID.
    pub requested_tool_confirmations: BTreeMap<String, ToolConfirmation>,
    /// The credential requests that tools made in this event, keyed by function call ID, since
    /// one function response event can answer several calls.
    ///
    /// Session services persist a request and the Agent Engine server sends it to clients
    /// unredacted, so it must not carry the secrets the tool was configured with: store the
    /// output of [`crate::toolauth::Config::without_secrets`]. The in-memory, database and
    /// Vertex AI session services refuse to append an event whose request does not decode as a
    /// [`crate::toolauth::Config`] (see [`crate::state_util::check_requested_auth_configs`]).
    pub requested_auth_configs: BTreeMap<String, crate::toolauth::Config>,
    /// If true, the model is not called to summarize the function response.
    pub skip_summarization: bool,
    /// If set, the event transfers to the named agent.
    pub transfer_to_agent: String,
    /// The agent is escalating to a higher-level agent.
    pub escalate: bool,
    /// Marks this event as a context-compaction summary; written by the framework only.
    pub compaction: Option<EventCompaction>,
}

/// JSON names of `EventActions`, for case-insensitive decoding.
const ACTIONS_KEYS: &[&str] = &[
    "stateDelta",
    "artifactDelta",
    "requestedToolConfirmations",
    "requestedAuthConfigs",
    "skipSummarization",
    "transferToAgent",
    "escalate",
    "compaction",
];

impl Serialize for EventActions {
    /// Writes keys in a fixed order: the promoted fields first, then the two delta-map
    /// fields.
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(None)?;
        if !self.requested_tool_confirmations.is_empty() {
            m.serialize_entry(
                "requestedToolConfirmations",
                &self.requested_tool_confirmations,
            )?;
        }
        if !self.requested_auth_configs.is_empty() {
            m.serialize_entry("requestedAuthConfigs", &self.requested_auth_configs)?;
        }
        if self.skip_summarization {
            m.serialize_entry("skipSummarization", &true)?;
        }
        if !self.transfer_to_agent.is_empty() {
            m.serialize_entry("transferToAgent", &self.transfer_to_agent)?;
        }
        if self.escalate {
            m.serialize_entry("escalate", &true)?;
        }
        if let Some(c) = &self.compaction {
            m.serialize_entry("compaction", c)?;
        }
        if let Some(d) = &self.state_delta {
            m.serialize_entry("stateDelta", d)?;
        }
        if let Some(d) = &self.artifact_delta {
            m.serialize_entry("artifactDelta", d)?;
        }
        m.end()
    }
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct EventActionsWire {
    state_delta: Option<Map<String, Value>>,
    artifact_delta: Option<BTreeMap<String, i64>>,
    requested_tool_confirmations: BTreeMap<String, ToolConfirmation>,
    requested_auth_configs: BTreeMap<String, crate::toolauth::Config>,
    skip_summarization: bool,
    transfer_to_agent: String,
    escalate: bool,
    compaction: Option<EventCompaction>,
}

impl<'de> Deserialize<'de> for EventActions {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let mut map = Map::<String, Value>::deserialize(d)?;
        normalize_keys(&mut map, ACTIONS_KEYS);
        drop_nulls(&mut map);
        let w: EventActionsWire =
            serde_json::from_value(Value::Object(map)).map_err(D::Error::custom)?;
        Ok(EventActions {
            state_delta: w.state_delta,
            artifact_delta: w.artifact_delta,
            requested_tool_confirmations: w.requested_tool_confirmations,
            requested_auth_configs: w.requested_auth_configs,
            skip_summarization: w.skip_summarization,
            transfer_to_agent: w.transfer_to_agent,
            escalate: w.escalate,
            compaction: w.compaction,
        })
    }
}

/// Records that a contiguous range of events has been replaced by one piece of content,
/// typically a model-generated summary. Both bounds are inclusive.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventCompaction {
    /// Timestamp of the earliest covered event (inclusive).
    #[serde(with = "go_time", default = "zero_time")]
    pub start_timestamp: DateTime<Utc>,
    /// Timestamp of the latest covered event (inclusive); never before `start_timestamp`.
    #[serde(with = "go_time", default = "zero_time")]
    pub end_timestamp: DateTime<Utc>,
    /// The content that replaces the covered events in the prompt (`null` when `None`).
    #[serde(default)]
    pub compacted_content: Option<Content>,
    /// Events inside the range that this summary does NOT stand in for.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub excluded_events: Vec<EventRef>,
}

/// Identifies a stored event by fields that survive a storage round trip.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventRef {
    /// The event's invocation ID.
    #[serde(default)]
    pub invocation_id: String,
    /// The event's timestamp.
    #[serde(with = "go_time", default = "zero_time")]
    pub timestamp: DateTime<Utc>,
}

/// Per-event metadata identifying the workflow node that emitted it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct NodeInfo {
    /// Composite path of the emitting node within its workflow activation.
    #[serde(skip_serializing_if = "is_zero")]
    pub path: String,
    /// This event's content is the node's output.
    #[serde(skip_serializing_if = "is_zero")]
    pub message_as_output: bool,
    /// Node paths this event's output counts for.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub output_for: Vec<String>,
}

/// A single human-in-the-loop prompt emitted by a workflow node.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct RequestInput {
    /// Correlates this request with the response that resumes it.
    pub interrupt_id: String,
    /// Human-readable description of what is being asked.
    #[serde(skip_serializing_if = "is_zero")]
    pub message: String,
    /// JSON schema the response payload must conform to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_schema: Option<Value>,
    /// Optional context the UI may render alongside the prompt.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn go_time_format_matches_rfc3339_nano() {
        assert_eq!(
            go_time::format(&zero_time()).as_deref(),
            Some("0001-01-01T00:00:00Z")
        );
        let t = DateTime::from_timestamp(1733040000, 500_000_000).unwrap_or_default();
        assert_eq!(
            go_time::format(&t).as_deref(),
            Some("2024-12-01T08:00:00.5Z")
        );
        let t = DateTime::from_timestamp(1733040000, 123_456_789).unwrap_or_default();
        assert_eq!(
            go_time::format(&t).as_deref(),
            Some("2024-12-01T08:00:00.123456789Z")
        );
    }

    #[test]
    fn event_actions_key_order_matches_go() {
        let a = EventActions {
            state_delta: Some(Map::new()),
            artifact_delta: Some(BTreeMap::new()),
            transfer_to_agent: "w".into(),
            escalate: true,
            ..Default::default()
        };
        assert_eq!(
            serde_json::to_string(&a).ok().as_deref(),
            Some(r#"{"transferToAgent":"w","escalate":true,"stateDelta":{},"artifactDelta":{}}"#)
        );
    }

    #[test]
    fn is_final_response_ports_go() {
        let mut e = Event::default();
        assert!(e.is_final_response());
        e.response.partial = true;
        assert!(!e.is_final_response());
        e.long_running_tool_ids = vec!["t".into()];
        assert!(e.is_final_response());
        e.actions.compaction = Some(EventCompaction {
            start_timestamp: zero_time(),
            end_timestamp: zero_time(),
            compacted_content: None,
            excluded_events: Vec::new(),
        });
        assert!(!e.is_final_response());
    }
}
