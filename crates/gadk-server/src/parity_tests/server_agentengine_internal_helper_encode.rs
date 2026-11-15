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

//! Ports of ADK-Go `server/agentengine/internal/helper/encode_test.go`.
//!
//! Go's tests call `convertSnake` on test-local structs; here those structs are local
//! [`GoStruct`] shapes over a `serde_json::Value` of their fields ([`Local`]); a local struct
//! that another local struct embeds is passed as an extra shape.

use std::collections::BTreeMap;

use bytes::Bytes;
use gadk_genai::{Content, Part};
use gadk_session::{Event, RequestInput};
use serde_json::{Value, json};

use crate::agentengine::Log;
use crate::agentengine::encode::{
    ConvertError, GoField, GoKind, GoSnake, GoStruct, GoValue, convert_name, convert_snake,
    convert_struct_with, emit_json,
};

/// A Go test-local struct value: its shape, the local shapes it reaches, and its fields keyed
/// by json name (or Go name).
struct Local {
    shape: &'static GoStruct,
    extra: &'static [&'static GoStruct],
    fields: Value,
}

impl GoSnake for Local {
    fn go_snake(&self, path: &str) -> Result<GoValue, ConvertError> {
        convert_struct_with(self.extra, self.shape, &self.fields, path)
    }
}

fn obj<const N: usize>(entries: [(&str, GoValue); N]) -> GoValue {
    GoValue::Object(
        entries
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v))
            .collect::<BTreeMap<_, _>>(),
    )
}

fn get<'a>(v: &'a GoValue, key: &str) -> Option<&'a GoValue> {
    match v {
        GoValue::Object(m) => m.get(key),
        _ => None,
    }
}

#[test]
fn test_names() {
    let tests = [
        ("a", "a"),
        ("A", "a"),
        ("aa", "aa"),
        ("Aa", "aa"),
        ("AaAa", "aa_aa"),
        ("ArtifactDelta", "artifact_delta"),
        ("RequestedToolConfirmations", "requested_tool_confirmations"),
        ("ID", "id"),
        ("InvocationID", "invocation_id"),
        // special case, handled by exception - see pathName
        ("LongRunningToolIDs", "long_running_tool_i_ds"),
    ];
    for (name, want) in tests {
        assert_eq!(convert_name("", name), want, "convertName({name:?})");
    }
}

fn assert_long_running_tool_ids(o: &GoValue) {
    let arr = get(o, "long_running_tool_ids").expect("long_running_tool_ids not found");
    assert_eq!(
        arr,
        &GoValue::Array(vec![
            GoValue::String("1".into()),
            GoValue::String("2".into())
        ]),
        "long_running_tool_ids"
    );
}

#[test]
fn test_event() {
    let event = Event {
        id: "1".into(),
        long_running_tool_ids: vec!["1".into(), "2".into()],
        ..Default::default()
    };
    let o = event.go_snake("").expect("convertSnake() failed");
    assert!(matches!(o, GoValue::Object(_)), "o is not a map");
    assert_long_running_tool_ids(&o);
}

#[test]
fn test_event_log_probs() {
    let mut event = Event {
        id: "1".into(),
        long_running_tool_ids: vec!["1".into(), "2".into()],
        ..Default::default()
    };
    event.response.content = Some(Content {
        parts: vec![Part {
            text: "Hello".into(),
            ..Default::default()
        }],
        ..Default::default()
    });
    let o = event.go_snake("").expect("convertSnake() failed");
    assert!(matches!(o, GoValue::Object(_)), "o is not a map");
    assert_long_running_tool_ids(&o);
}

#[test]
fn test_byte_slice() {
    let mut event = Event {
        id: "1".into(),
        ..Default::default()
    };
    event.response.content = Some(Content {
        parts: vec![Part {
            text: "hi".into(),
            thought_signature: Bytes::from_static(&[0xDE, 0xAD, 0xBE, 0xEF]),
            ..Default::default()
        }],
        ..Default::default()
    });
    let o = convert_snake(&event).expect("convertSnake() failed");
    let content = get(&o, "content").expect("content is not a map");
    let Some(GoValue::Array(parts)) = get(content, "parts") else {
        panic!("parts is not a slice: {content:?}");
    };
    assert_eq!(parts.len(), 1, "parts is not a 1-element slice");
    assert_eq!(
        get(&parts[0], "thought_signature"),
        Some(&GoValue::String("3q2+7w==".into())),
        "thought_signature"
    );
}

/// Go `type A struct { Sig []byte \`json:"sig,omitempty"\` }`.
const BYTES_A: GoStruct = GoStruct {
    go_type: "helper.A",
    fields: &[GoField {
        name: "Sig",
        tag: "sig,omitempty",
        embedded: false,
        exported: true,
        go_type: "[]uint8",
        kind: GoKind::Bytes,
    }],
};

#[test]
fn test_byte_slice_omit_empty() {
    let a = Local {
        shape: &BYTES_A,
        extra: &[],
        fields: json!({}),
    };
    let got = a.go_snake("").expect("convertSnake() failed");
    assert!(matches!(got, GoValue::Object(_)), "got is not a map");
    assert_eq!(
        get(&got, "sig"),
        None,
        "sig should be omitted for an empty []byte"
    );
}

/// Go `type A struct { Default json.RawMessage \`json:"default,omitempty"\` }`.
const RAW_A: GoStruct = GoStruct {
    go_type: "helper.A",
    fields: &[GoField {
        name: "Default",
        tag: "default,omitempty",
        embedded: false,
        exported: true,
        go_type: "json.RawMessage",
        kind: GoKind::RawMessage,
    }],
};

#[test]
fn test_raw_message() {
    let a = Local {
        shape: &RAW_A,
        extra: &[],
        fields: json!({"default": {"approved": false}}),
    };
    let got = a.go_snake("").expect("convertSnake() failed");
    let b = gadk_model::to_go_json(&got).expect("json.Marshal() failed");
    assert_eq!(
        String::from_utf8_lossy(&b),
        r#"{"default":{"approved":false}}"#
    );
}

#[test]
fn test_raw_message_omit_empty() {
    let a = Local {
        shape: &RAW_A,
        extra: &[],
        fields: json!({}),
    };
    let got = a.go_snake("").expect("convertSnake() failed");
    assert!(matches!(got, GoValue::Object(_)), "got is not a map");
    assert_eq!(
        get(&got, "default"),
        None,
        "default should be omitted for an empty json.RawMessage"
    );
}

/// Drives `emit_json` with a real `session.Event` carrying a `jsonschema.Schema`, so a
/// `json.RawMessage` field can no longer make `ConvertSnake` fall back to its camelCase input
/// unnoticed.
#[test]
fn test_emit_json_schema_snake_case() {
    let event = Event {
        id: "1".into(),
        requested_input: Some(RequestInput {
            interrupt_id: "abc".into(),
            response_schema: Some(json!({
                "default": {"approved": false},
                "properties": {"approved": {"default": true}},
            })),
            ..Default::default()
        }),
        ..Default::default()
    };
    let lines = std::sync::Arc::new(parking_lot::Mutex::new(Vec::<String>::new()));
    let sink = lines.clone();
    let log = Log(Some(std::sync::Arc::new(move |s: &str| {
        sink.lock().push(s.to_owned())
    })));
    let body = emit_json(&event, &log).expect("EmitJSON() failed");
    assert!(
        lines.lock().is_empty(),
        "conversion fell back: {:?}",
        lines.lock()
    );

    let got: Value = serde_json::from_slice(&body).expect("json.Unmarshal failed");
    assert!(
        got.get("requestedInput").is_none(),
        "output fell back to camelCase key requestedInput: {got}"
    );
    let requested_input = got
        .get("requested_input")
        .and_then(Value::as_object)
        .expect("requested_input missing or not an object");
    let response_schema = requested_input
        .get("response_schema")
        .and_then(Value::as_object)
        .expect("response_schema missing or not an object");
    assert_eq!(
        response_schema.get("default"),
        Some(&json!({"approved": false})),
        "response_schema.default mismatch"
    );
    let properties = response_schema
        .get("properties")
        .and_then(Value::as_object)
        .expect("properties missing or not an object");
    let approved = properties
        .get("approved")
        .and_then(Value::as_object)
        .expect("properties.approved missing or not an object");
    assert_eq!(
        approved.get("default"),
        Some(&json!(true)),
        "properties.approved.default"
    );
}

/// Go `type A struct { anInteger int; aString string }`.
const EMBEDDED_A: GoStruct = GoStruct {
    go_type: "helper.A",
    fields: &[
        GoField {
            name: "anInteger",
            tag: "",
            embedded: false,
            exported: false,
            go_type: "int",
            kind: GoKind::Int,
        },
        GoField {
            name: "aString",
            tag: "",
            embedded: false,
            exported: false,
            go_type: "string",
            kind: GoKind::String,
        },
    ],
};

/// Go `type B struct { A; anotherString string }`.
const EMBEDDED_B: GoStruct = GoStruct {
    go_type: "helper.B",
    fields: &[
        GoField {
            name: "A",
            tag: "",
            embedded: true,
            exported: true,
            go_type: "helper.A",
            kind: GoKind::Struct("helper.A"),
        },
        GoField {
            name: "anotherString",
            tag: "",
            embedded: false,
            exported: false,
            go_type: "string",
            kind: GoKind::String,
        },
    ],
};

#[test]
fn test_embedded() {
    let b = Local {
        shape: &EMBEDDED_B,
        extra: &[&EMBEDDED_A],
        fields: json!({"anInteger": 1, "aString": "a", "anotherString": "b"}),
    };
    let got = b.go_snake("").expect("convertSnake() failed");
    let want = obj([
        ("an_integer", GoValue::Int(1)),
        ("a_string", GoValue::String("a".into())),
        ("another_string", GoValue::String("b".into())),
    ]);
    assert_eq!(got, want);
}

/// Go `type A struct { OmittableArr []int \`json:"optional_array,omitempty"\`; MandatoryArr
/// []int \`json:"must_array"\`; OmmitableIntToIntMap map[int]int
/// \`json:"optional_map,omitempty"\`; MandatoryIntToIntMap map[int]int \`json:"must_map"\` }`.
const OMIT_A: GoStruct = GoStruct {
    go_type: "helper.A",
    fields: &[
        GoField {
            name: "OmittableArr",
            tag: "optional_array,omitempty",
            embedded: false,
            exported: true,
            go_type: "[]int",
            kind: GoKind::Slice(&GoKind::Int),
        },
        GoField {
            name: "MandatoryArr",
            tag: "must_array",
            embedded: false,
            exported: true,
            go_type: "[]int",
            kind: GoKind::Slice(&GoKind::Int),
        },
        GoField {
            name: "OmmitableIntToIntMap",
            tag: "optional_map,omitempty",
            embedded: false,
            exported: true,
            go_type: "map[int]int",
            kind: GoKind::Map(&GoKind::Int),
        },
        GoField {
            name: "MandatoryIntToIntMap",
            tag: "must_map",
            embedded: false,
            exported: true,
            go_type: "map[int]int",
            kind: GoKind::Map(&GoKind::Int),
        },
    ],
};

#[test]
fn test_omit_empty() {
    let tests = [
        (
            "nil, nil, nil, nil",
            json!({}),
            obj([
                ("must_array", GoValue::Array(vec![])),
                ("must_map", GoValue::Object(BTreeMap::new())),
            ]),
        ),
        (
            "empty, nil, nil, nil",
            json!({"optional_array": []}),
            obj([
                ("must_array", GoValue::Array(vec![])),
                ("must_map", GoValue::Object(BTreeMap::new())),
            ]),
        ),
        (
            "nil, [1], nil, nil",
            json!({"optional_array": [1]}),
            obj([
                ("must_array", GoValue::Array(vec![])),
                ("must_map", GoValue::Object(BTreeMap::new())),
                ("optional_array", GoValue::Array(vec![GoValue::Int(1)])),
            ]),
        ),
    ];
    for (name, fields, want) in tests {
        let a = Local {
            shape: &OMIT_A,
            extra: &[],
            fields,
        };
        let got = a.go_snake("").expect("convertSnake() failed");
        assert_eq!(got, want, "{name}");
    }
}

#[test]
// parity: adapted: gadk converts a `time.Time` only as a struct field (there is no GoSnake for a
// bare chrono time), so each time is converted as a session event's `timestamp`
#[ignore = "8a bug: convertSnake writes time.Time as integer Unix seconds (UnixMilli() / 1000); Go c00807e6 (#2199) writes float64(UnixMicro()) / 1e6, keeping microseconds"]
fn test_time_fractional_seconds() {
    let converted_timestamp = |t: chrono::DateTime<chrono::Utc>| {
        let event = Event {
            timestamp: t,
            ..Default::default()
        };
        let o = event.go_snake("").expect("convertSnake() failed");
        get(&o, "timestamp").cloned()
    };

    // Verify microsecond resolution is preserved (e.g., matching adkrest UnixMicro / 1e6)
    let want = 1733040000.123456;
    let got =
        converted_timestamp(chrono::DateTime::from_timestamp_micros(1733040000123456).unwrap());
    assert_eq!(
        got,
        Some(GoValue::Float(want)),
        "convertSnake(time.Time), want float64 {want}"
    );

    // Verify whole-second timestamps
    let got_whole = converted_timestamp(chrono::DateTime::from_timestamp(1700000000, 0).unwrap());
    assert_eq!(
        got_whole,
        Some(GoValue::Float(1700000000.0)),
        "convertSnake(whole second), want 1700000000.0"
    );

    // Verify EmitJSON serialization output
    let event = Event {
        id: "event-1".into(),
        timestamp: chrono::DateTime::from_timestamp_micros(1733040000123456).unwrap(),
        ..Default::default()
    };
    let body = emit_json(&event, &Log(None)).expect("EmitJSON() failed");
    let json_map: Value = serde_json::from_slice(&body).expect("json.Unmarshal failed");
    let ts = json_map["timestamp"]
        .as_f64()
        .unwrap_or_else(|| panic!("timestamp missing or not float64 in JSON: {json_map}"));
    assert_eq!(ts, want, "EmitJSON timestamp");
}
