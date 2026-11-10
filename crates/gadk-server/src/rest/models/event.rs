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

//! Port of Go `models/event.go`: the REST view of a session event.

use std::collections::BTreeMap;

use gadk_genai::serde_util::is_zero;
use gadk_genai::{
    Content, FinishReason, GenerateContentResponseUsageMetadata, GroundingMetadata, Part,
    Transcription,
};
use gadk_session::{EventCompaction, NodeInfo, RequestInput, ToolConfirmation};
use serde::ser::{Error as _, SerializeMap};
use serde::{Deserialize, Serialize, Serializer};
use serde_json::value::RawValue;
use serde_json::{Map, Value};

use super::null_default;

/// Port of Go `models.EventActions` (plain struct tags: `stateDelta` and `artifactDelta` are
/// written as `null` when unset).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct EventActions {
    /// Go `StateDelta map[string]any` (`null` when `None`).
    pub state_delta: Option<Map<String, Value>>,
    /// Go `ArtifactDelta map[string]int64` (`null` when `None`).
    pub artifact_delta: Option<BTreeMap<String, i64>>,
    /// `omitempty`.
    #[serde(skip_serializing_if = "is_zero", deserialize_with = "null_default")]
    pub escalate: bool,
    /// `omitempty`.
    #[serde(skip_serializing_if = "is_zero", deserialize_with = "null_default")]
    pub skip_summarization: bool,
    /// `omitempty`.
    #[serde(skip_serializing_if = "is_zero", deserialize_with = "null_default")]
    pub transfer_to_agent: String,
    /// `omitempty`.
    #[serde(
        skip_serializing_if = "BTreeMap::is_empty",
        deserialize_with = "null_default"
    )]
    pub requested_tool_confirmations: BTreeMap<String, ToolConfirmation>,
    /// `omitempty`; read-only over REST: [`Event::to_session_event`] drops it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compaction: Option<EventCompaction>,
}

/// Port of Go `models.Event`. Serialization is Go's `Event.MarshalJSON`: the fields in Go's order
/// with `content` last, and inside it `role` before `parts`, each part's `functionCall` last and
/// that call's `args` last and always present (`{}` when empty).
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Event {
    /// Go `ID`.
    #[serde(deserialize_with = "null_default")]
    pub id: String,
    /// Go `InvocationID`.
    #[serde(deserialize_with = "null_default")]
    pub invocation_id: String,
    /// `omitempty`.
    #[serde(deserialize_with = "null_default")]
    pub branch: String,
    /// Go `Author`.
    #[serde(deserialize_with = "null_default")]
    pub author: String,
    /// `omitempty`.
    #[serde(deserialize_with = "null_default")]
    pub partial: bool,
    /// Go `LongRunningToolIDs` (`longRunningToolIds`), `omitempty`.
    #[serde(deserialize_with = "null_default")]
    pub long_running_tool_ids: Vec<String>,
    /// `null` when `None`.
    pub content: Option<Content>,
    /// `null` when `None`.
    pub grounding_metadata: Option<GroundingMetadata>,
    /// `null` when `None`.
    pub usage_metadata: Option<GenerateContentResponseUsageMetadata>,
    /// `omitempty`.
    #[serde(deserialize_with = "null_default")]
    pub turn_complete: bool,
    /// `omitempty`.
    #[serde(deserialize_with = "null_default")]
    pub interrupted: bool,
    /// `omitempty`.
    #[serde(deserialize_with = "null_default")]
    pub error_code: String,
    /// `omitempty`.
    #[serde(deserialize_with = "null_default")]
    pub error_message: String,
    /// `omitempty`.
    #[serde(deserialize_with = "null_default")]
    pub avg_logprobs: f64,
    /// `omitempty`.
    pub finish_reason: Option<FinishReason>,
    /// `omitempty`.
    #[serde(deserialize_with = "null_default")]
    pub model_version: String,
    /// `omitempty`.
    pub input_transcription: Option<Transcription>,
    /// `omitempty`.
    pub output_transcription: Option<Transcription>,
    /// Go `Actions`.
    #[serde(deserialize_with = "null_default")]
    pub actions: EventActions,
    /// Go `Output any`, `omitempty` (omitted when `None` or JSON `null`).
    pub output: Option<Value>,
    /// `omitempty`.
    pub node_info: Option<NodeInfo>,
    /// `omitempty`.
    #[serde(deserialize_with = "null_default")]
    pub isolation_scope: String,
    /// `omitempty`.
    #[serde(deserialize_with = "null_default")]
    pub routes: Vec<String>,
    /// `omitempty`.
    pub requested_input: Option<RequestInput>,
}

impl Event {
    /// Port of Go `models.FromSessionEvent` (the timestamp is not part of the REST view).
    pub fn from_session_event(ev: &gadk_session::Event) -> Event {
        let r = &ev.response;
        Event {
            id: ev.id.clone(),
            invocation_id: ev.invocation_id.clone(),
            branch: ev.branch.clone(),
            author: ev.author.clone(),
            partial: r.partial,
            long_running_tool_ids: ev.long_running_tool_ids.clone(),
            content: r.content.clone(),
            grounding_metadata: r.grounding_metadata.clone(),
            usage_metadata: r.usage_metadata.clone(),
            turn_complete: r.turn_complete,
            interrupted: r.interrupted,
            error_code: r.error_code.clone(),
            error_message: r.error_message.clone(),
            avg_logprobs: r.avg_logprobs,
            finish_reason: r.finish_reason.clone(),
            model_version: r.model_version.clone(),
            input_transcription: r.input_transcription.clone(),
            output_transcription: r.output_transcription.clone(),
            actions: EventActions {
                state_delta: ev.actions.state_delta.clone(),
                artifact_delta: ev.actions.artifact_delta.clone(),
                escalate: ev.actions.escalate,
                skip_summarization: ev.actions.skip_summarization,
                transfer_to_agent: ev.actions.transfer_to_agent.clone(),
                requested_tool_confirmations: ev.actions.requested_tool_confirmations.clone(),
                compaction: ev.actions.compaction.clone(),
            },
            output: ev.output.clone(),
            node_info: ev.node_info.clone(),
            isolation_scope: ev.isolation_scope.clone(),
            routes: ev.routes.clone(),
            requested_input: ev.requested_input.clone(),
        }
    }

    /// Port of Go `models.ToSessionEvent`. `actions.compaction` is deliberately dropped: a
    /// client-supplied compaction record would let it erase history and speak into the gap.
    pub fn to_session_event(&self) -> gadk_session::Event {
        let mut ev = gadk_session::Event {
            id: self.id.clone(),
            invocation_id: self.invocation_id.clone(),
            branch: self.branch.clone(),
            author: self.author.clone(),
            long_running_tool_ids: self.long_running_tool_ids.clone(),
            output: self.output.clone(),
            node_info: self.node_info.clone(),
            isolation_scope: self.isolation_scope.clone(),
            routes: self.routes.clone(),
            requested_input: self.requested_input.clone(),
            ..Default::default()
        };
        let r = &mut ev.response;
        r.avg_logprobs = self.avg_logprobs;
        r.content = self.content.clone();
        r.grounding_metadata = self.grounding_metadata.clone();
        r.usage_metadata = self.usage_metadata.clone();
        r.partial = self.partial;
        r.turn_complete = self.turn_complete;
        r.interrupted = self.interrupted;
        r.error_code = self.error_code.clone();
        r.error_message = self.error_message.clone();
        r.finish_reason = self.finish_reason.clone();
        r.model_version = self.model_version.clone();
        r.input_transcription = self.input_transcription.clone();
        r.output_transcription = self.output_transcription.clone();
        ev.actions = gadk_session::EventActions {
            state_delta: self.actions.state_delta.clone(),
            artifact_delta: self.actions.artifact_delta.clone(),
            escalate: self.actions.escalate,
            skip_summarization: self.actions.skip_summarization,
            transfer_to_agent: self.actions.transfer_to_agent.clone(),
            requested_tool_confirmations: self.actions.requested_tool_confirmations.clone(),
            compaction: None,
        };
        ev
    }
}

impl Serialize for Event {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(None)?;
        m.serialize_entry("id", &self.id)?;
        m.serialize_entry("invocationId", &self.invocation_id)?;
        if !self.branch.is_empty() {
            m.serialize_entry("branch", &self.branch)?;
        }
        m.serialize_entry("author", &self.author)?;
        if self.partial {
            m.serialize_entry("partial", &true)?;
        }
        if !self.long_running_tool_ids.is_empty() {
            m.serialize_entry("longRunningToolIds", &self.long_running_tool_ids)?;
        }
        m.serialize_entry("groundingMetadata", &self.grounding_metadata)?;
        m.serialize_entry("usageMetadata", &self.usage_metadata)?;
        if self.turn_complete {
            m.serialize_entry("turnComplete", &true)?;
        }
        if self.interrupted {
            m.serialize_entry("interrupted", &true)?;
        }
        if !self.error_code.is_empty() {
            m.serialize_entry("errorCode", &self.error_code)?;
        }
        if !self.error_message.is_empty() {
            m.serialize_entry("errorMessage", &self.error_message)?;
        }
        if self.avg_logprobs != 0.0 {
            m.serialize_entry("avgLogprobs", &self.avg_logprobs)?;
        }
        if let Some(f) = &self.finish_reason {
            m.serialize_entry("finishReason", f)?;
        }
        if !self.model_version.is_empty() {
            m.serialize_entry("modelVersion", &self.model_version)?;
        }
        if let Some(t) = &self.input_transcription {
            m.serialize_entry("inputTranscription", t)?;
        }
        if let Some(t) = &self.output_transcription {
            m.serialize_entry("outputTranscription", t)?;
        }
        m.serialize_entry("actions", &self.actions)?;
        if let Some(o) = self.output.as_ref().filter(|o| !o.is_null()) {
            m.serialize_entry("output", o)?;
        }
        if let Some(n) = &self.node_info {
            m.serialize_entry("nodeInfo", n)?;
        }
        if !self.isolation_scope.is_empty() {
            m.serialize_entry("isolationScope", &self.isolation_scope)?;
        }
        if !self.routes.is_empty() {
            m.serialize_entry("routes", &self.routes)?;
        }
        if let Some(r) = &self.requested_input {
            m.serialize_entry("requestedInput", r)?;
        }
        let content = match &self.content {
            Some(c) => Some(proxy_content(c).map_err(S::Error::custom)?),
            None => None,
        };
        m.serialize_entry("content", &content)?;
        m.end()
    }
}

/// Go `to_go_json` bytes of `v` as a string.
fn go_json<T: Serialize + ?Sized>(v: &T) -> Result<String, serde_json::Error> {
    let b = gadk_model::to_go_json(v)?;
    Ok(String::from_utf8_lossy(&b).into_owned())
}

/// `obj` (a JSON object's text) with `"key":raw` appended as its last member.
fn append_member(obj: &str, key: &str, raw: &str) -> String {
    let inner = obj
        .strip_prefix('{')
        .and_then(|o| o.strip_suffix('}'))
        .unwrap_or_default();
    if inner.is_empty() {
        format!("{{\"{key}\":{raw}}}")
    } else {
        format!("{{{inner},\"{key}\":{raw}}}")
    }
}

/// Go `ProxyContent`: the embedded `*genai.Content`'s `role` (its `parts` is shadowed) before
/// the proxy's own `parts`.
fn proxy_content(c: &Content) -> Result<Box<RawValue>, serde_json::Error> {
    let mut members = Vec::new();
    if !c.role.is_empty() {
        members.push(format!("\"role\":{}", go_json(&c.role)?));
    }
    if !c.parts.is_empty() {
        let parts = c
            .parts
            .iter()
            .map(proxy_part)
            .collect::<Result<Vec<_>, _>>()?;
        members.push(format!("\"parts\":[{}]", parts.join(",")));
    }
    RawValue::from_string(format!("{{{}}}", members.join(",")))
}

/// Go `ProxyPart`: the part's own fields, then `functionCall` (Go `ProxyFunctionCall`: the call's
/// fields, then `args`, `{}` when empty).
fn proxy_part(p: &Part) -> Result<String, serde_json::Error> {
    let Some(fc) = &p.function_call else {
        return go_json(p);
    };
    let mut rest = p.clone();
    rest.function_call = None;
    let mut call = fc.clone();
    call.args = Map::new();
    let call = append_member(&go_json(&call)?, "args", &go_json(&fc.args)?);
    Ok(append_member(&go_json(&rest)?, "functionCall", &call))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use gadk_genai::FunctionCall;
    use serde_json::json;

    use super::*;

    fn wire(ev: &Event) -> String {
        String::from_utf8(gadk_model::to_go_json(ev).unwrap()).unwrap()
    }

    /// Go `json.Marshal(models.Event{ID: "e", Author: "a"})`.
    #[test]
    fn zero_event_bytes_match_go() {
        let ev = Event {
            id: "e".into(),
            author: "a".into(),
            ..Default::default()
        };
        assert_eq!(
            wire(&ev),
            r#"{"id":"e","invocationId":"","author":"a","groundingMetadata":null,"usageMetadata":null,"actions":{"stateDelta":null,"artifactDelta":null},"content":null}"#
        );
    }

    /// Go `json.Marshal` of an event whose model content has a text part and a function call
    /// without args, plus omitempty fields: `content` last, `role` before `parts`,
    /// `functionCall` last in its part and `args` last (and `{}`) in the call.
    #[test]
    fn content_proxy_bytes_match_go() {
        let ev = Event {
            id: "e1".into(),
            invocation_id: "inv".into(),
            author: "model".into(),
            branch: "b".into(),
            turn_complete: true,
            avg_logprobs: -0.5,
            content: Some(Content {
                role: "model".into(),
                parts: vec![
                    Part {
                        text: "<hi>".into(),
                        ..Default::default()
                    },
                    Part {
                        thought: true,
                        function_call: Some(FunctionCall {
                            id: "c1".into(),
                            name: "get".into(),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                ],
            }),
            actions: EventActions {
                state_delta: Some(Map::from_iter([("k".to_owned(), json!(1))])),
                escalate: true,
                ..Default::default()
            },
            output: Some(json!("out")),
            routes: vec!["r".into()],
            ..Default::default()
        };
        assert_eq!(
            wire(&ev),
            concat!(
                r#"{"id":"e1","invocationId":"inv","branch":"b","author":"model","#,
                r#""groundingMetadata":null,"usageMetadata":null,"turnComplete":true,"#,
                r#""avgLogprobs":-0.5,"actions":{"stateDelta":{"k":1},"artifactDelta":null,"escalate":true},"#,
                r#""output":"out","routes":["r"],"#,
                r#""content":{"role":"model","parts":[{"text":"\u003chi\u003e"},"#,
                r#"{"thought":true,"functionCall":{"id":"c1","name":"get","args":{}}}]}}"#
            )
        );
    }

    #[test]
    fn empty_content_writes_an_empty_object() {
        let ev = Event {
            content: Some(Content::default()),
            ..Default::default()
        };
        assert!(wire(&ev).ends_with(r#""content":{}}"#));
    }

    #[test]
    fn decodes_go_output_and_nulls() {
        let ev: Event = serde_json::from_str(
            r#"{"id":"e","invocationId":null,"author":"a","groundingMetadata":null,"usageMetadata":null,
                "actions":{"stateDelta":null,"artifactDelta":{"f":2}},"routes":null,
                "content":{"role":"user","parts":[{"text":"x"}]},"unknown":1}"#,
        )
        .unwrap();
        assert_eq!(ev.id, "e");
        assert_eq!(ev.invocation_id, "");
        assert_eq!(
            ev.actions.artifact_delta,
            Some(BTreeMap::from([("f".into(), 2)]))
        );
        assert_eq!(ev.content.unwrap().parts[0].text, "x");
    }
}
