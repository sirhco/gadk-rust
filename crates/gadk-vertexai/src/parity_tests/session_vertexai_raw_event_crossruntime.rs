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

//! Ports of ADK-Go `session/vertexai/raw_event_crossruntime_test.go`: `rawEvent` is shared
//! with other ADK runtimes, so its keys are camelCase and a dump written by adk-python reads
//! back.

use chrono::DateTime;
use gadk_genai::{Content, Transcription};
use gadk_model::LlmResponse;
use gadk_session::{Event, EventActions, NodeInfo, RequestInput};
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

use crate::session::convert::{event_from_raw_event, event_to_raw_event};

#[test]
fn test_raw_event_wire_keys_are_camel_case() {
    let event = Event {
        response: LlmResponse {
            content: Some(Content::from_text("hi", "model")),
            input_transcription: Some(Transcription {
                text: "hello".into(),
                ..Default::default()
            }),
            output_transcription: Some(Transcription {
                text: "hi".into(),
                ..Default::default()
            }),
            ..Default::default()
        },
        id: "evt-1".into(),
        timestamp: DateTime::from_timestamp(1_733_040_000, 0).unwrap(),
        invocation_id: "inv-1".into(),
        branch: "root.planner".into(),
        author: "planner".into(),
        isolation_scope: "approval_subflow".into(),
        long_running_tool_ids: vec!["tool-1".into()],
        routes: vec!["reviewer".into()],
        requested_input: Some(RequestInput {
            interrupt_id: "approval-1".into(),
            message: "Approve?".into(),
            ..Default::default()
        }),
        output: Some(json!({"summary": "ok"})),
        node_info: Some(NodeInfo {
            path: "wf/planner".into(),
            message_as_output: true,
            ..Default::default()
        }),
        actions: EventActions {
            state_delta: Some(json!({"stage": "drafted"}).as_object().unwrap().clone()),
            ..Default::default()
        },
    };

    let raw = event_to_raw_event(&event).unwrap();
    let mut got: Vec<&str> = raw
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    got.sort_unstable();

    let want = [
        "actions",
        "author",
        "branch",
        "content",
        "id",
        "inputTranscription",
        "invocationId",
        "isolationScope",
        "longRunningToolIds",
        "nodeInfo",
        "output",
        "outputTranscription",
        "requestedInput",
        "routes",
        "timestamp",
    ];
    assert_eq!(got, want, "raw_event wire keys mismatch");
}

/// A raw_event payload as emitted by adk-python, whose Event model dumps camelCase aliases and
/// carries its timestamp as a float (Go's `pythonRawEvent`).
const PYTHON_RAW_EVENT: &str = r#"{
  "actions": {"artifactDelta": {"artifact": 1.0}, "escalate": true,
              "requestedAuthConfigs": {}, "requestedToolConfirmations": {},
              "skipSummarization": true, "stateDelta": {"state": "updated"},
              "transferToAgent": "test-agent"},
  "author": "user",
  "branch": "test-branch",
  "content": {"parts": [{"text": "Hello, raw_event!"},
                        {"functionCall": {"args": {"arg1": "value1"},
                                          "id": "test-id", "name": "test-function"}}],
              "role": "user"},
  "customMetadata": {"custom-key": "custom-value"},
  "errorCode": "400",
  "errorMessage": "test error",
  "groundingMetadata": {"retrievalQueries": ["query2"], "webSearchQueries": ["query1"]},
  "id": "test-id",
  "interrupted": true,
  "invocationId": "test-invocation-id",
  "isolationScope": "scope-1",
  "longRunningToolIds": ["test-tool-id-1", "test-tool-id-2"],
  "nodeInfo": {"messageAsOutput": true, "outputFor": ["wf/A@1/B@1", "wf/A@1"],
               "path": "wf/A@1/B@1"},
  "output": {"count": 3.0, "result": "ok"},
  "partial": true,
  "timestamp": 1733040000.0,
  "turnComplete": true,
  "usageMetadata": {"candidatesTokenCount": 50.0, "promptTokenCount": 100.0,
                    "totalTokenCount": 200.0}
}"#;

#[test]
fn test_event_from_raw_event_decodes_other_runtime_dump() {
    let raw: Value = serde_json::from_str(PYTHON_RAW_EVENT).expect("test fixture is valid JSON");

    let got = event_from_raw_event(&raw).unwrap();

    // The timestamp arrives as a float; decoding must not abort on it and leave every later
    // field unset.
    assert_eq!(
        got.timestamp,
        DateTime::from_timestamp(1_733_040_000, 0).unwrap(),
        "Timestamp"
    );

    let scalars: [(&str, Value, Value); 12] = [
        ("Author", json!(got.author), json!("user")),
        ("Branch", json!(got.branch), json!("test-branch")),
        (
            "InvocationID",
            json!(got.invocation_id),
            json!("test-invocation-id"),
        ),
        (
            "IsolationScope",
            json!(got.isolation_scope),
            json!("scope-1"),
        ),
        ("ErrorCode", json!(got.response.error_code), json!("400")),
        (
            "ErrorMessage",
            json!(got.response.error_message),
            json!("test error"),
        ),
        ("Partial", json!(got.response.partial), json!(true)),
        (
            "TurnComplete",
            json!(got.response.turn_complete),
            json!(true),
        ),
        ("Interrupted", json!(got.response.interrupted), json!(true)),
        (
            "Actions.TransferToAgent",
            json!(got.actions.transfer_to_agent),
            json!("test-agent"),
        ),
        ("Actions.Escalate", json!(got.actions.escalate), json!(true)),
        (
            "Actions.SkipSummarization",
            json!(got.actions.skip_summarization),
            json!(true),
        ),
    ];
    for (name, got, want) in scalars {
        assert_eq!(got, want, "{name} mismatch");
    }

    let want_node = NodeInfo {
        path: "wf/A@1/B@1".into(),
        message_as_output: true,
        output_for: vec!["wf/A@1/B@1".into(), "wf/A@1".into()],
    };
    assert_eq!(got.node_info, Some(want_node), "NodeInfo mismatch");
    // Go: map[string]any{"count": float64(3), …}; a float64 3 is the JSON number 3.
    assert_eq!(
        got.output,
        Some(json!({"count": 3, "result": "ok"})),
        "Output mismatch"
    );
    assert_eq!(
        got.actions.state_delta,
        Some(json!({"state": "updated"}).as_object().unwrap().clone()),
        "Actions.StateDelta mismatch"
    );
    assert_eq!(
        got.long_running_tool_ids,
        vec!["test-tool-id-1".to_string(), "test-tool-id-2".to_string()],
        "LongRunningToolIDs mismatch"
    );

    let content = got.response.content.expect("Content = None, want 2 parts");
    assert_eq!(
        content.parts.len(),
        2,
        "Content = {content:?}, want 2 parts"
    );
    assert_eq!(content.parts[0].text, "Hello, raw_event!");
    let fc = content.parts[1]
        .function_call
        .as_ref()
        .expect("Content.Parts[1].FunctionCall = None, want a decoded function call");
    assert_eq!(
        (fc.name.as_str(), fc.id.as_str()),
        ("test-function", "test-id"),
        "FunctionCall"
    );
}
