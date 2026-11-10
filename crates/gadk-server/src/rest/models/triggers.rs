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

//! Port of Go `models/triggers.go`: Pub/Sub push and Eventarc (CloudEvents) envelopes.

use std::collections::BTreeMap;

use bytes::Bytes;
use gadk_genai::serde_util::base64_bytes;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::value::RawValue;

use super::null_default;

/// Port of Go `models.PubSubTriggerRequest`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PubSubTriggerRequest {
    /// The Pub/Sub message.
    #[serde(deserialize_with = "null_default")]
    pub message: PubSubMessage,
    /// The subscription the message was published to.
    #[serde(deserialize_with = "null_default")]
    pub subscription: String,
}

/// Port of Go `models.PubSubMessage`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PubSubMessage {
    /// The payload (base64 on the wire).
    #[serde(with = "base64_bytes")]
    pub data: Bytes,
    /// Go `MessageID`.
    #[serde(deserialize_with = "null_default")]
    pub message_id: String,
    /// Go `PublishTime`.
    #[serde(deserialize_with = "null_default")]
    pub publish_time: String,
    /// `omitempty`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attributes: Option<BTreeMap<String, String>>,
    /// `omitempty`.
    #[serde(
        skip_serializing_if = "String::is_empty",
        deserialize_with = "null_default"
    )]
    pub ordering_key: String,
}

/// Port of Go `models.EventarcTriggerRequest` (structured-mode CloudEvent).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct EventarcTriggerRequest {
    /// CloudEvents `id`.
    #[serde(deserialize_with = "null_default")]
    pub id: String,
    /// CloudEvents `source`.
    #[serde(deserialize_with = "null_default")]
    pub source: String,
    /// CloudEvents `type`.
    #[serde(rename = "type", deserialize_with = "null_default")]
    pub type_: String,
    /// CloudEvents `specversion`.
    #[serde(rename = "specversion", deserialize_with = "null_default")]
    pub spec_version: String,
    /// `omitempty`.
    #[serde(
        skip_serializing_if = "String::is_empty",
        deserialize_with = "null_default"
    )]
    pub time: String,
    /// Go `json.RawMessage`, `omitempty`: absent is `None`; a JSON `null` is kept as the raw text
    /// `null` (as Go's `RawMessage` keeps it).
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "raw_keep_null"
    )]
    pub data: Option<Box<RawValue>>,
}

/// Decodes a present member as its raw text, `null` included.
fn raw_keep_null<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Box<RawValue>>, D::Error> {
    Box::<RawValue>::deserialize(d).map(Some)
}

/// Port of Go `models.TriggerResponse`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TriggerResponse {
    /// `success` or an error message.
    pub status: String,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn pubsub_envelope_decodes_base64_data() {
        let req: PubSubTriggerRequest = serde_json::from_str(
            r#"{"message":{"data":"aGk=","messageId":"m","publishTime":"t","attributes":{"k":"v"}},"subscription":"s"}"#,
        )
        .unwrap();
        assert_eq!(req.message.data.as_ref(), b"hi");
        assert_eq!(req.message.attributes.unwrap()["k"], "v");
        assert_eq!(req.subscription, "s");
    }

    #[test]
    fn eventarc_data_keeps_raw_text_and_null() {
        let req: EventarcTriggerRequest =
            serde_json::from_str(r#"{"id":"1","type":"t","specversion":"1.0","data":{"a": 1}}"#)
                .unwrap();
        assert_eq!(req.data.unwrap().get(), r#"{"a": 1}"#);
        let null: EventarcTriggerRequest = serde_json::from_str(r#"{"data":null}"#).unwrap();
        assert_eq!(null.data.unwrap().get(), "null");
        let absent: EventarcTriggerRequest = serde_json::from_str("{}").unwrap();
        assert!(absent.data.is_none());
    }

    #[test]
    fn trigger_response_bytes() {
        let r = TriggerResponse {
            status: "success".into(),
        };
        assert_eq!(
            gadk_model::to_go_json(&r).unwrap(),
            br#"{"status":"success"}"#
        );
    }
}
