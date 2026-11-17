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

//! The `/run`, `/run_sse` and `/run_live` request bodies.

use bytes::Bytes;
use gadk_genai::serde_util::base64_bytes;
use gadk_genai::{ActivityEnd, ActivityStart, Content};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::null_default;

/// The `/run` and `/run_sse` request body; unknown fields are rejected.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RunAgentRequest {
    /// The application name.
    #[serde(default, deserialize_with = "null_default")]
    pub app_name: String,
    /// The user ID.
    #[serde(default, deserialize_with = "null_default")]
    pub user_id: String,
    /// The session ID.
    #[serde(default, deserialize_with = "null_default")]
    pub session_id: String,
    /// The message that starts the run.
    #[serde(default, deserialize_with = "null_default")]
    pub new_message: Content,
    /// Omitted from the JSON when empty.
    #[serde(
        default,
        skip_serializing_if = "std::ops::Not::not",
        deserialize_with = "null_default"
    )]
    pub streaming: bool,
    /// State changes applied with the run; omitted from the JSON when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state_delta: Option<Map<String, Value>>,
    /// Accepted for adk-python parity and ignored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub function_call_event_id: Option<String>,
}

impl RunAgentRequest {
    /// `<name> is required` for the first zero field of `appName`, `userId`, `sessionId`,
    /// `newMessage`. The handlers do not call it; only tests do.
    #[cfg(test)]
    pub fn assert_run_agent_request_required(&self) -> Result<(), String> {
        let zero = [
            ("appName", self.app_name.is_empty()),
            ("userId", self.user_id.is_empty()),
            ("sessionId", self.session_id.is_empty()),
            ("newMessage", self.new_message == Content::default()),
        ];
        match zero.iter().find(|(_, z)| *z) {
            Some((name, _)) => Err(format!("{name} is required")),
            None => Ok(()),
        }
    }
}

/// A media chunk from a live client (`mime_type`, base64 `data`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LiveBlob {
    /// Omitted from the JSON when empty.
    #[serde(
        default,
        skip_serializing_if = "String::is_empty",
        deserialize_with = "null_default"
    )]
    pub mime_type: String,
    /// Omitted from the JSON when empty.
    #[serde(
        default,
        skip_serializing_if = "Bytes::is_empty",
        with = "base64_bytes"
    )]
    pub data: Bytes,
}

/// One client message on a `/run_live` WebSocket.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LiveRequest {
    /// Omitted from the JSON when empty.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<Content>,
    /// Omitted from the JSON when empty.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blob: Option<LiveBlob>,
    /// Omitted from the JSON when empty.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub activity_start: Option<ActivityStart>,
    /// Omitted from the JSON when empty.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub activity_end: Option<ActivityEnd>,
    /// Omitted from the JSON when empty.
    #[serde(
        skip_serializing_if = "std::ops::Not::not",
        deserialize_with = "null_default"
    )]
    pub close: bool,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn run_agent_request_rejects_unknown_fields() {
        let err = serde_json::from_str::<RunAgentRequest>(r#"{"appName":"a","bogus":1}"#)
            .unwrap_err()
            .to_string();
        assert!(err.contains("unknown field `bogus`"), "{err}");
        let ok: RunAgentRequest = serde_json::from_str(
            r#"{"appName":"a","userId":"u","sessionId":"s","newMessage":{"role":"user","parts":[{"text":"hi"}]},"functionCallEventId":"e","stateDelta":{}}"#,
        )
        .unwrap();
        assert_eq!(ok.state_delta, Some(Map::new()));
        assert!(ok.assert_run_agent_request_required().is_ok());
    }

    #[test]
    fn required_fields_report_go_text() {
        let mut req = RunAgentRequest::default();
        assert_eq!(
            req.assert_run_agent_request_required().unwrap_err(),
            "appName is required"
        );
        req.app_name = "a".into();
        req.user_id = "u".into();
        req.session_id = "s".into();
        assert_eq!(
            req.assert_run_agent_request_required().unwrap_err(),
            "newMessage is required"
        );
    }

    #[test]
    fn live_request_decodes_blob_and_signals() {
        let req: LiveRequest = serde_json::from_str(
            r#"{"blob":{"mime_type":"audio/pcm","data":"AAE="},"activityStart":{},"close":true}"#,
        )
        .unwrap();
        let blob = req.blob.unwrap();
        assert_eq!(
            (blob.mime_type.as_str(), blob.data.as_ref()),
            ("audio/pcm", &[0u8, 1][..])
        );
        assert!(req.activity_start.is_some() && req.activity_end.is_none() && req.close);
    }
}
