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

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Port of ADK-Go `session/event_json_test.go`.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use gadk_genai::{Content, ROLE_MODEL};
use gadk_model::LlmResponse;
use gadk_platform::Platform;
use gadk_session::*;
use pretty_assertions::assert_eq;
use serde_json::{Map, Value, json};

fn ts(secs: i64, nanos: u32) -> DateTime<Utc> {
    DateTime::from_timestamp(secs, nanos).unwrap()
}

fn obj(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        other => panic!("not an object: {other}"),
    }
}

fn fully_populated_event() -> Event {
    Event {
        response: LlmResponse {
            content: Some(Content::from_text("hello", ROLE_MODEL)),
            model_version: "gemini-3-pro".into(),
            partial: true,
            turn_complete: true,
            interrupted: true,
            error_code: "429".into(),
            error_message: "rate limited".into(),
            avg_logprobs: -0.25,
            ..Default::default()
        },
        id: "evt-1".into(),
        timestamp: ts(1733040000, 0),
        invocation_id: "inv-1".into(),
        branch: "root.planner".into(),
        isolation_scope: "approval_subflow".into(),
        author: "planner".into(),
        long_running_tool_ids: vec!["tool-1".into(), "tool-2".into()],
        routes: vec!["reviewer".into(), "writer".into()],
        requested_input: Some(RequestInput {
            interrupt_id: "approval-1".into(),
            message: "Approve?".into(),
            payload: Some(json!({"doc": "contract.pdf"})),
            ..Default::default()
        }),
        // Go uses float64(3); serde_json keeps integer and float numbers distinct, so the
        // round trip is only exact for the integer form (which is also what Go writes).
        output: Some(json!({"summary": "ok", "risk_items": 3})),
        node_info: Some(NodeInfo {
            path: "wf/planner".into(),
            message_as_output: true,
            output_for: vec!["wf".into()],
        }),
        actions: EventActions {
            state_delta: Some(obj(json!({"stage": "drafted"}))),
            artifact_delta: Some(BTreeMap::from([("report.pdf".to_string(), 2)])),
            skip_summarization: true,
            transfer_to_agent: "writer".into(),
            escalate: true,
            ..Default::default()
        },
    }
}

fn sorted_keys(v: &Value) -> Vec<String> {
    let mut keys: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
    keys.sort();
    keys
}

/// Pins the wire format: Event is persisted whole and read back by other ADK runtimes.
#[test]
fn test_event_json_keys() {
    let b = serde_json::to_vec(&fully_populated_event()).unwrap();
    let m: Value = serde_json::from_slice(&b).unwrap();
    let want = [
        "actions",
        "author",
        "avgLogprobs",
        "branch",
        "content",
        "errorCode",
        "errorMessage",
        "id",
        "interrupted",
        "invocationId",
        "isolationScope",
        "longRunningToolIds",
        "modelVersion",
        "nodeInfo",
        "output",
        "partial",
        "requestedInput",
        "routes",
        "timestamp",
        "turnComplete",
    ];
    assert_eq!(sorted_keys(&m), want);
}

/// Guards against every event carrying a full set of nulls.
#[test]
fn test_event_json_omits_empty() {
    let b = serde_json::to_vec(&Event::default()).unwrap();
    let m: Value = serde_json::from_slice(&b).unwrap();
    assert_eq!(
        sorted_keys(&m),
        ["actions", "author", "id", "invocationId", "timestamp"]
    );
    // Go writes a zero time.Time (no omitempty) as its RFC 3339 form.
    assert_eq!(m["timestamp"], json!("0001-01-01T00:00:00Z"));
    assert_eq!(m["actions"], json!({}));
}

#[test]
fn test_event_json_round_trip() {
    let want = fully_populated_event();
    let b = serde_json::to_vec(&want).unwrap();
    let got: Event = serde_json::from_slice(&b).unwrap();
    assert_eq!(got, want);
}

/// The three timestamp encodings an Event can arrive in: RFC 3339, epoch seconds as a
/// number (adk-python), and absent.
#[test]
fn test_event_unmarshal_timestamp_dialects() {
    let cases: &[(&str, &str, DateTime<Utc>)] = &[
        (
            "rfc3339 string",
            r#"{"id":"e","timestamp":"2024-12-01T08:00:00Z"}"#,
            ts(1733040000, 0),
        ),
        (
            "epoch seconds number",
            r#"{"id":"e","timestamp":1733040000}"#,
            ts(1733040000, 0),
        ),
        (
            "fractional epoch seconds",
            r#"{"id":"e","timestamp":1733040000.5}"#,
            ts(1733040000, 500_000_000),
        ),
        (
            "millisecond precision",
            r#"{"id":"e","timestamp":1733040000.123}"#,
            ts(1733040000, 123_000_000),
        ),
        (
            "exponent notation",
            r#"{"id":"e","timestamp":1.7330400e9}"#,
            ts(1733040000, 0),
        ),
        ("zero epoch", r#"{"id":"e","timestamp":0}"#, ts(0, 0)),
        (
            "negative epoch",
            r#"{"id":"e","timestamp":-86400}"#,
            ts(-86400, 0),
        ),
        (
            "negative fractional epoch",
            r#"{"id":"e","timestamp":-0.5}"#,
            ts(-1, 500_000_000),
        ),
        ("absent", r#"{"id":"e"}"#, zero_time()),
        ("null", r#"{"id":"e","timestamp":null}"#, zero_time()),
    ];
    for (name, input, want) in cases {
        let got: Event = serde_json::from_str(input)
            .unwrap_or_else(|e| panic!("{name}: Unmarshal({input}) failed: {e}"));
        assert_eq!(got.timestamp, *want, "{name}: timestamp");
        assert_eq!(got.id, "e", "{name}: decoding must not abort early");
    }
}

/// Timestamps that cannot be decoded must surface an error rather than a zero field.
#[test]
fn test_event_unmarshal_timestamp_invalid() {
    for input in [
        r#"{"timestamp":"not-a-date"}"#,
        r#"{"timestamp":{"nested":1}}"#,
        r#"{"timestamp":true}"#,
        // Out of range for int64 microseconds.
        r#"{"timestamp":1e300}"#,
        r#"{"timestamp":-1e300}"#,
        r#"{"timestamp":1e19}"#,
        r#"{"timestamp":9223372036854775807}"#,
        // Epoch milliseconds read as seconds.
        r#"{"timestamp":1733040000000}"#,
    ] {
        assert!(
            serde_json::from_str::<Event>(input).is_err(),
            "Unmarshal({input}) = Ok, want an error"
        );
    }
}

/// Rows written before Event carried json tags used Go field names verbatim; Go's
/// encoding/json matches keys case-insensitively, so these must still bind.
#[test]
fn test_event_unmarshal_legacy_field_names() {
    let legacy = r#"{
      "ID":"evt-1","InvocationID":"inv-1","Author":"planner","Branch":"root",
      "Timestamp":"2024-12-01T08:00:00Z","Routes":["reviewer"],
      "Output":{"summary":"ok"},"LongRunningToolIDs":["tool-1"],
      "Actions":{"StateDelta":{"stage":"drafted"},"TransferToAgent":"writer"},
      "Partial":true,"ErrorCode":"429"
    }"#;
    let got: Event = serde_json::from_str(legacy).unwrap();
    let want = Event {
        response: LlmResponse {
            partial: true,
            error_code: "429".into(),
            ..Default::default()
        },
        id: "evt-1".into(),
        timestamp: DateTime::from_timestamp(1733040000, 0).unwrap(),
        invocation_id: "inv-1".into(),
        branch: "root".into(),
        author: "planner".into(),
        long_running_tool_ids: vec!["tool-1".into()],
        routes: vec!["reviewer".into()],
        output: Some(json!({"summary": "ok"})),
        actions: EventActions {
            state_delta: Some(serde_json::Map::from_iter([(
                "stage".into(),
                json!("drafted"),
            )])),
            transfer_to_agent: "writer".into(),
            ..Default::default()
        },
        ..Default::default()
    };
    assert_eq!(got, want);
}

/// Go decodes into an already-populated Event and leaves absent fields alone. Rust has no
/// "decode into" for owned values, so this keeps the Go case table and checks that a fresh
/// decode yields exactly the fields present in the input, whichever timestamp form is used.
#[test]
fn test_event_unmarshal_into_populated_event() {
    for (name, input) in [
        (
            "string timestamp",
            r#"{"id":"new","timestamp":"2024-12-01T08:00:00Z"}"#,
        ),
        (
            "numeric timestamp",
            r#"{"id":"new","timestamp":1733040000}"#,
        ),
    ] {
        let got: Event = serde_json::from_str(input).unwrap();
        let want = Event {
            id: "new".into(),
            timestamp: ts(1733040000, 0),
            ..Default::default()
        };
        assert_eq!(got, want, "{name}");
    }
}

/// adk-python writes a nodeInfo object on every event; an all-zero one means "absent".
#[test]
fn test_event_unmarshal_empty_node_info_becomes_nil() {
    let cases: Vec<(&str, &str, Option<NodeInfo>)> = vec![
        ("absent", r#"{"id":"e"}"#, None),
        (
            "empty object, as adk-python writes for a non-workflow event",
            r#"{"id":"e","nodeInfo":{}}"#,
            None,
        ),
        (
            "explicitly zero fields",
            r#"{"id":"e","nodeInfo":{"path":"","messageAsOutput":false,"outputFor":[]}}"#,
            None,
        ),
        (
            "a path alone is real workflow metadata",
            r#"{"id":"e","nodeInfo":{"path":"wf/planner"}}"#,
            Some(NodeInfo {
                path: "wf/planner".into(),
                ..Default::default()
            }),
        ),
        (
            "messageAsOutput alone is real workflow metadata",
            r#"{"id":"e","nodeInfo":{"messageAsOutput":true}}"#,
            Some(NodeInfo {
                message_as_output: true,
                ..Default::default()
            }),
        ),
        (
            "outputFor alone is real workflow metadata",
            r#"{"id":"e","nodeInfo":{"outputFor":["wf"]}}"#,
            Some(NodeInfo {
                output_for: vec!["wf".into()],
                ..Default::default()
            }),
        ),
    ];
    for (name, input, want) in cases {
        let got: Event = serde_json::from_str(input)
            .unwrap_or_else(|e| panic!("{name}: Unmarshal({input}) failed: {e}"));
        assert_eq!(got.node_info, want, "{name}");
    }
}

/// Event::new allocates both delta maps; an allocated-but-empty map must survive a round
/// trip as `Some(empty)`, not `None`.
#[test]
fn test_event_json_preserves_empty_action_maps() {
    let want = Event::new(&Platform::default(), "inv-1");
    assert!(
        want.actions.state_delta.is_some() && want.actions.artifact_delta.is_some(),
        "Event::new no longer allocates the action maps; this test is no longer covering anything"
    );
    let b = serde_json::to_vec(&want).unwrap();
    let got: Event = serde_json::from_slice(&b).unwrap();
    assert!(
        got.actions.state_delta.is_some(),
        "StateDelta is None after a round trip"
    );
    assert!(
        got.actions.artifact_delta.is_some(),
        "ArtifactDelta is None after a round trip"
    );
}

/// A `None` delta map is omitted and a `Some` one is written even when empty; `null` is
/// never emitted (adk-python rejects it) and the round trip preserves None-ness exactly.
#[test]
fn test_event_actions_json_delta_map_encoding() {
    /// json key -> raw value; `None` means absent.
    type WantKeys = [(&'static str, Option<&'static str>); 2];
    let cases: Vec<(&str, EventActions, WantKeys)> = vec![
        (
            "nil maps are omitted",
            EventActions::default(),
            [("stateDelta", None), ("artifactDelta", None)],
        ),
        (
            "allocated but empty maps are written",
            EventActions {
                state_delta: Some(Map::new()),
                artifact_delta: Some(BTreeMap::new()),
                ..Default::default()
            },
            [("stateDelta", Some("{}")), ("artifactDelta", Some("{}"))],
        ),
        (
            "populated maps are written",
            EventActions {
                state_delta: Some(obj(json!({"k": "v"}))),
                artifact_delta: Some(BTreeMap::from([("f.png".to_string(), 7)])),
                ..Default::default()
            },
            [
                ("stateDelta", Some(r#"{"k":"v"}"#)),
                ("artifactDelta", Some(r#"{"f.png":7}"#)),
            ],
        ),
        (
            "one allocated, one nil",
            EventActions {
                state_delta: Some(Map::new()),
                ..Default::default()
            },
            [("stateDelta", Some("{}")), ("artifactDelta", None)],
        ),
    ];

    for (name, actions, want) in cases {
        let ev = Event {
            id: "e".into(),
            author: "a".into(),
            actions: actions.clone(),
            ..Default::default()
        };
        let b = serde_json::to_vec(&ev).unwrap();
        let wire: Value = serde_json::from_slice(&b).unwrap();
        let wire_actions = wire["actions"].as_object().unwrap();
        for (key, want_raw) in want {
            let got_raw = wire_actions
                .get(key)
                .map(|v| serde_json::to_string(v).unwrap());
            assert_eq!(got_raw.as_deref(), want_raw, "{name}: actions.{key}");
        }
        for key in ["stateDelta", "artifactDelta"] {
            assert_ne!(
                wire_actions.get(key),
                Some(&Value::Null),
                "{name}: actions.{key} = null"
            );
        }
        let got: Event = serde_json::from_slice(&b).unwrap();
        assert_eq!(
            got.actions.state_delta.is_none(),
            actions.state_delta.is_none(),
            "{name}: StateDelta None-ness after round trip"
        );
        assert_eq!(
            got.actions.artifact_delta.is_none(),
            actions.artifact_delta.is_none(),
            "{name}: ArtifactDelta None-ness after round trip"
        );
    }
}
