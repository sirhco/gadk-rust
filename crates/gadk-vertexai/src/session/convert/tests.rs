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

//! Conversion tests against Go: golden JSON produced by go1.27 programs over ADK-Go's own
//! conversion functions (`createAiplatformpb*`, `eventToRawEvent`, `aiplatformToGenaiContent`,
//! `aiplatformToSessionEventActions`, `createGroundingMetadata`) marshaled with protojson
//! `UseEnumNumbers` (Go's REST client form), and the `AppendEvent` / `ListEvents` bodies Go
//! sent and received in the 22 recorded conversations.

use std::collections::BTreeMap;

use bytes::Bytes;
use chrono::{DateTime, Duration, TimeZone, Utc};
use gadk_genai::{
    Blob, Citation, CitationMetadata, CodeExecutionResult, Content, ExecutableCode, FileData,
    FunctionCall, FunctionResponse, GenerateContentResponseUsageMetadata, GroundingChunk,
    GroundingChunkMaps, GroundingChunkMapsPlaceAnswerSources,
    GroundingChunkMapsPlaceAnswerSourcesReviewSnippet, GroundingChunkRetrievedContext,
    GroundingChunkWeb, GroundingMetadata, GroundingSupport, Language, Outcome, Part, RAGChunk,
    RAGChunkPageSpan, RetrievalMetadata, SearchEntryPoint, Segment,
};
use gadk_model::LlmResponse;
use gadk_session::{Event, EventActions, EventCompaction, EventRef, NodeInfo, zero_time};
use pretty_assertions::assert_eq;
use serde_json::{Map, Value, json};

use super::*;

fn obj(v: Value) -> Map<String, Value> {
    v.as_object().expect("an object").clone()
}

fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32, nanos: i64) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(y, mo, d, h, mi, s).unwrap() + Duration::nanoseconds(nanos)
}

/// An event as Go's JSON writes it, read back as a value (through bytes, as Go compares).
fn go_json(event: &Event) -> Value {
    serde_json::from_slice(&serde_json::to_vec(event).unwrap()).unwrap()
}

// ---- Struct numbers ----

#[test]
fn go_double_reads_back_as_go_writes_it() {
    for (f, want) in [
        (3.0, json!(3)),
        (-3.0, json!(-3)),
        (1.5, json!(1.5)),
        (9_007_199_254_740_993_f64, json!(9_007_199_254_740_992_i64)),
        (1e20, json!(100_000_000_000_000_000_000.0)),
        (1e21, json!(1e21)),
        (0.25, json!(0.25)),
        (-0.0, json!(-0.0)),
    ] {
        assert_eq!(go_double(f), want, "go_double({f})");
    }
    // An integral double from 2^63 to 2^64 reads back as a u64.
    assert_eq!(go_double(1.8e19), json!(18_000_000_000_000_000_000_u64));
}

#[test]
fn go_float32_uses_the_shortest_decimal() {
    assert_eq!(go_float32(0.9), json!(0.9));
    assert_eq!(go_float32(2.0), json!(2));
    assert_eq!(go_float32(1e-7), json!(1e-7));
    assert_eq!(go_float32(f32::NAN), json!("NaN"));
    assert_eq!(go_float32(f32::INFINITY), json!("Infinity"));
    assert_eq!(go_float32(f32::NEG_INFINITY), json!("-Infinity"));
}

#[test]
fn to_struct_matches_go() {
    // Go toStructPB(map[string]any{"count": 3, "big": int64(1<<53 + 1), "nested": …}).
    let got = to_struct(&json!({
        "count": 3, "big": 9_007_199_254_740_993_i64, "f": 1.5, "nil": null,
        "nested": {"list": [1, "two", true, 2.0]}
    }))
    .unwrap();
    assert_eq!(
        got,
        json!({
            "count": 3, "big": 9_007_199_254_740_992_i64, "f": 1.5, "nil": null,
            "nested": {"list": [1, "two", true, 2]}
        })
    );
    assert_eq!(to_struct(&Value::Null).unwrap(), json!({}));
    // Texts from go1.27: toStructPB("hello"), toStructPB(5), toStructPB([]any{1}), toStructPB(true).
    for (v, token) in [
        (json!("hello"), r#""hello""#),
        (json!(5), "5"),
        (json!([1]), "["),
        (json!(true), "true"),
    ] {
        assert_eq!(
            to_struct(&v).unwrap_err().to_string(),
            format!(
                "failed to unmarshal JSON data to structpb: proto: syntax error (line 1:1): unexpected token {token}"
            )
        );
    }
}

#[test]
fn filter_nil_values_drops_nulls() {
    assert_eq!(
        filter_nil_values(&obj(json!({"a": 1, "b": null, "c": {"d": null}}))),
        obj(json!({"a": 1, "c": {"d": null}}))
    );
}

// ---- Go golden: a rich event ----

/// The event of the Go program's `richEvent()`, field for field.
fn rich_event() -> Event {
    Event {
        id: "evt-rich".into(),
        timestamp: utc(2026, 3, 4, 5, 6, 7, 120_000_000),
        invocation_id: "inv-rich".into(),
        author: "model-agent".into(),
        branch: "root.child".into(),
        long_running_tool_ids: vec!["lr-1".into(), "lr-2".into()],
        actions: EventActions {
            state_delta: Some(obj(json!({
                "count": 3,
                "big": 9_007_199_254_740_993_i64,
                "ratio": 1.5,
                "nothing": null,
                "nested": {"list": [1, "two", true]}
            }))),
            artifact_delta: Some(BTreeMap::from([("a.txt".into(), 0), ("b.png".into(), 2)])),
            ..Default::default()
        },
        response: LlmResponse {
            content: Some(Content {
                role: "model".into(),
                parts: vec![
                    Part {
                        text: "thinking".into(),
                        thought: true,
                        thought_signature: Bytes::from_static(b"sig"),
                        ..Default::default()
                    },
                    Part {
                        inline_data: Some(Blob {
                            mime_type: "image/png".into(),
                            data: Bytes::from_static(&[0, 1, 2, 250]),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                    Part {
                        inline_data: Some(Blob::default()),
                        ..Default::default()
                    },
                    Part {
                        function_call: Some(FunctionCall {
                            id: "c1".into(),
                            name: "f".into(),
                            args: obj(json!({"n": 7, "x": null})),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                    Part {
                        function_call: Some(FunctionCall {
                            name: "noargs".into(),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                    Part {
                        function_response: Some(FunctionResponse {
                            id: "c1".into(),
                            name: "f".into(),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                    Part {
                        function_response: Some(FunctionResponse {
                            name: "f2".into(),
                            response: obj(json!({"ok": true})),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                    Part::from_executable_code("print(1)", Language::Python),
                    Part {
                        executable_code: Some(ExecutableCode {
                            code: "x".into(),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                    Part::from_code_execution_result(Outcome::DeadlineExceeded, "slow"),
                    Part {
                        code_execution_result: Some(CodeExecutionResult::default()),
                        ..Default::default()
                    },
                    Part::from_uri("gs://b/f.txt", "text/plain"),
                    Part {
                        text: "overridden".into(),
                        file_data: Some(FileData {
                            file_uri: "gs://b/g".into(),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                    Part::default(),
                ],
            }),
            turn_complete: true,
            interrupted: true,
            error_code: "E1".into(),
            error_message: "boom".into(),
            custom_metadata: obj(json!({"k": "v", "n": 2, "f": 0.25})),
            grounding_metadata: Some(GroundingMetadata {
                web_search_queries: vec!["q1".into(), "q2".into()],
                retrieval_queries: vec!["r1".into()],
                google_maps_widget_context_token: "tok".into(),
                search_entry_point: Some(SearchEntryPoint {
                    rendered_content: "<div/>".into(),
                    sdk_blob: Bytes::from_static(b"blob"),
                }),
                retrieval_metadata: Some(RetrievalMetadata {
                    google_search_dynamic_retrieval_score: 0.9,
                }),
                grounding_chunks: vec![
                    GroundingChunk {
                        web: Some(GroundingChunkWeb {
                            uri: "https://w".into(),
                            title: "W".into(),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                    GroundingChunk {
                        retrieved_context: Some(GroundingChunkRetrievedContext {
                            uri: "u".into(),
                            title: "t".into(),
                            text: "x".into(),
                            document_name: "d".into(),
                            rag_chunk: Some(RAGChunk {
                                text: "rag".into(),
                                page_span: Some(RAGChunkPageSpan {
                                    first_page: 1,
                                    last_page: 3,
                                }),
                                ..Default::default()
                            }),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                    GroundingChunk {
                        retrieved_context: Some(GroundingChunkRetrievedContext {
                            rag_chunk: Some(RAGChunk {
                                text: "no-span".into(),
                                ..Default::default()
                            }),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                    GroundingChunk {
                        maps: Some(GroundingChunkMaps {
                            uri: "m".into(),
                            title: "M".into(),
                            text: "mt".into(),
                            place_id: "p1".into(),
                            place_answer_sources: Some(GroundingChunkMapsPlaceAnswerSources {
                                review_snippets: vec![
                                    GroundingChunkMapsPlaceAnswerSourcesReviewSnippet {
                                        review: "rev".into(),
                                        review_id: "rid".into(),
                                        google_maps_uri: "gm".into(),
                                        title: "rt".into(),
                                        ..Default::default()
                                    },
                                ],
                                ..Default::default()
                            }),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                    GroundingChunk {
                        maps: Some(GroundingChunkMaps::default()),
                        web: Some(GroundingChunkWeb::default()),
                        ..Default::default()
                    },
                    GroundingChunk::default(),
                ],
                grounding_supports: vec![
                    GroundingSupport {
                        grounding_chunk_indices: vec![0, 1],
                        confidence_scores: vec![0.9, 0.25],
                        segment: Some(Segment {
                            part_index: 1,
                            start_index: 2,
                            end_index: 5,
                            text: "seg".into(),
                        }),
                        ..Default::default()
                    },
                    GroundingSupport::default(),
                ],
                ..Default::default()
            }),
            ..Default::default()
        },
        ..Default::default()
    }
}

/// Go: `protojson.MarshalOptions{UseEnumNumbers: true}.Marshal` of the `SessionEvent`
/// `appendEvent` builds for `richEvent()`.
const GO_RICH_SESSION_EVENT: &str = r#"{
  "actions": {
    "artifactDelta": {
      "a.txt": 0,
      "b.png": 2
    },
    "stateDelta": {
      "big": 9007199254740992,
      "count": 3,
      "nested": {
        "list": [
          1,
          "two",
          true
        ]
      },
      "nothing": null,
      "ratio": 1.5
    }
  },
  "author": "model-agent",
  "content": {
    "parts": [
      {
        "text": "thinking",
        "thought": true,
        "thoughtSignature": "c2ln"
      },
      {
        "inlineData": {
          "data": "AAEC+g==",
          "mimeType": "image/png"
        }
      },
      {
        "inlineData": {}
      },
      {
        "functionCall": {
          "args": {
            "n": 7,
            "x": null
          },
          "id": "c1",
          "name": "f"
        }
      },
      {
        "functionCall": {
          "args": {},
          "name": "noargs"
        }
      },
      {
        "functionResponse": {
          "id": "c1",
          "name": "f",
          "response": {}
        }
      },
      {
        "functionResponse": {
          "name": "f2",
          "response": {
            "ok": true
          }
        }
      },
      {
        "executableCode": {
          "code": "print(1)",
          "language": 1
        }
      },
      {
        "executableCode": {
          "code": "x"
        }
      },
      {
        "codeExecutionResult": {
          "outcome": 3,
          "output": "slow"
        }
      },
      {
        "codeExecutionResult": {}
      },
      {
        "fileData": {
          "fileUri": "gs://b/f.txt",
          "mimeType": "text/plain"
        }
      },
      {
        "fileData": {
          "fileUri": "gs://b/g"
        }
      },
      {}
    ],
    "role": "model"
  },
  "errorCode": "E1",
  "errorMessage": "boom",
  "eventMetadata": {
    "branch": "root.child",
    "customMetadata": {
      "f": 0.25,
      "k": "v",
      "n": 2
    },
    "groundingMetadata": {
      "googleMapsWidgetContextToken": "tok",
      "groundingChunks": [
        {
          "web": {
            "title": "W",
            "uri": "https://w"
          }
        },
        {
          "retrievedContext": {
            "documentName": "d",
            "ragChunk": {
              "pageSpan": {
                "firstPage": 1,
                "lastPage": 3
              },
              "text": "rag"
            },
            "text": "x",
            "title": "t",
            "uri": "u"
          }
        },
        {
          "retrievedContext": {
            "documentName": "",
            "text": "",
            "title": "",
            "uri": ""
          }
        },
        {
          "maps": {
            "placeAnswerSources": {
              "reviewSnippets": [
                {
                  "googleMapsUri": "gm",
                  "reviewId": "rev",
                  "title": "rt"
                }
              ]
            },
            "placeId": "p1",
            "text": "mt",
            "title": "M",
            "uri": "m"
          }
        },
        {
          "maps": {
            "placeId": "",
            "text": "",
            "title": "",
            "uri": ""
          }
        },
        {
          "web": {
            "title": "",
            "uri": ""
          }
        }
      ],
      "groundingSupports": [
        {
          "confidenceScores": [
            0.9,
            0.25
          ],
          "groundingChunkIndices": [
            0,
            1
          ],
          "segment": {
            "endIndex": 5,
            "partIndex": 1,
            "startIndex": 2,
            "text": "seg"
          }
        },
        {}
      ],
      "retrievalMetadata": {
        "googleSearchDynamicRetrievalScore": 0.9
      },
      "retrievalQueries": [
        "r1"
      ],
      "searchEntryPoint": {
        "renderedContent": "<div/>",
        "sdkBlob": "YmxvYg=="
      },
      "webSearchQueries": [
        "q1",
        "q2"
      ]
    },
    "interrupted": true,
    "longRunningToolIds": [
      "lr-1",
      "lr-2"
    ],
    "turnComplete": true
  },
  "invocationId": "inv-rich",
  "timestamp": "2026-03-04T05:06:07.120Z"
}"#;

/// Go: `json.Marshal` of the event `listSessionEvents` rebuilds from that `SessionEvent`
/// (legacy path, no rawEvent), with ID `"id"`.
const GO_RICH_DECODED_EVENT: &str = r#"{
  "actions": {
    "artifactDelta": {
      "a.txt": 0,
      "b.png": 2
    },
    "stateDelta": {
      "big": 9007199254740992,
      "count": 3,
      "nested": {
        "list": [
          1,
          "two",
          true
        ]
      },
      "ratio": 1.5
    }
  },
  "author": "model-agent",
  "branch": "root.child",
  "content": {
    "parts": [
      {
        "text": "thinking",
        "thought": true,
        "thoughtSignature": "c2ln"
      },
      {
        "inlineData": {
          "data": "AAEC+g==",
          "mimeType": "image/png"
        }
      },
      {
        "inlineData": {}
      },
      {
        "functionCall": {
          "args": {
            "n": 7,
            "x": null
          },
          "id": "c1",
          "name": "f"
        }
      },
      {
        "functionCall": {
          "name": "noargs"
        }
      },
      {
        "functionResponse": {
          "id": "c1",
          "name": "f"
        }
      },
      {
        "functionResponse": {
          "name": "f2",
          "response": {
            "ok": true
          }
        }
      },
      {
        "executableCode": {
          "code": "print(1)",
          "language": "PYTHON"
        }
      },
      {
        "executableCode": {
          "code": "x",
          "language": "LANGUAGE_UNSPECIFIED"
        }
      },
      {
        "codeExecutionResult": {
          "outcome": "OUTCOME_DEADLINE_EXCEEDED",
          "output": "slow"
        }
      },
      {
        "codeExecutionResult": {
          "outcome": "OUTCOME_UNSPECIFIED"
        }
      },
      {
        "fileData": {
          "fileUri": "gs://b/f.txt",
          "mimeType": "text/plain"
        }
      },
      {
        "fileData": {
          "fileUri": "gs://b/g"
        }
      },
      {}
    ],
    "role": "model"
  },
  "customMetadata": {
    "f": 0.25,
    "k": "v",
    "n": 2
  },
  "errorCode": "E1",
  "errorMessage": "boom",
  "groundingMetadata": {
    "googleMapsWidgetContextToken": "tok",
    "groundingChunks": [
      {
        "web": {
          "title": "W",
          "uri": "https://w"
        }
      },
      {
        "retrievedContext": {
          "documentName": "d",
          "ragChunk": {
            "pageSpan": {
              "firstPage": 1,
              "lastPage": 3
            },
            "text": "rag"
          },
          "text": "x",
          "title": "t",
          "uri": "u"
        }
      },
      {
        "retrievedContext": {}
      },
      {
        "maps": {
          "placeAnswerSources": {
            "reviewSnippets": [
              {
                "googleMapsUri": "gm",
                "review": "rev"
              }
            ]
          },
          "placeId": "p1",
          "text": "mt",
          "title": "M",
          "uri": "m"
        }
      },
      {
        "maps": {}
      },
      {
        "web": {}
      }
    ],
    "groundingSupports": [
      {
        "confidenceScores": [
          0.9,
          0.25
        ],
        "groundingChunkIndices": [
          0,
          1
        ],
        "segment": {
          "endIndex": 5,
          "partIndex": 1,
          "startIndex": 2,
          "text": "seg"
        }
      },
      {}
    ],
    "retrievalMetadata": {
      "googleSearchDynamicRetrievalScore": 0.9
    },
    "retrievalQueries": [
      "r1"
    ],
    "searchEntryPoint": {
      "renderedContent": "<div/>",
      "sdkBlob": "YmxvYg=="
    },
    "webSearchQueries": [
      "q1",
      "q2"
    ]
  },
  "id": "id",
  "interrupted": true,
  "invocationId": "inv-rich",
  "longRunningToolIds": [
    "lr-1",
    "lr-2"
  ],
  "timestamp": "2026-03-04T05:06:07.12Z",
  "turnComplete": true
}"#;

/// Go: the `SessionEvent` for an event that needs a rawEvent (workflow fields, a transcription,
/// an `Output` of `int64(1<<53 + 1)`).
const GO_RAW_SESSION_EVENT: &str = r#"{
  "actions": {
    "artifactDelta": {
      "f": 7
    },
    "stateDelta": {
      "count": 3
    }
  },
  "author": "agent-x",
  "eventMetadata": {},
  "invocationId": "inv-raw",
  "rawEvent": {
    "actions": {
      "artifactDelta": {
        "f": 7
      },
      "stateDelta": {
        "count": 3
      }
    },
    "author": "agent-x",
    "id": "evt-raw",
    "inputTranscription": {
      "text": "in"
    },
    "invocationId": "inv-raw",
    "isolationScope": "scope",
    "nodeInfo": {
      "messageAsOutput": true,
      "path": "wf/a"
    },
    "output": 9007199254740992,
    "routes": [
      "r"
    ],
    "timestamp": "2026-03-04T05:06:07.123456789Z",
    "usageMetadata": {
      "promptTokenCount": 10,
      "totalTokenCount": 12
    }
  },
  "timestamp": "2026-03-04T05:06:07.123456789Z"
}"#;

#[test]
fn event_to_session_event_matches_go_for_a_rich_event() {
    let want: Value = serde_json::from_str(GO_RICH_SESSION_EVENT).unwrap();
    assert_eq!(event_to_session_event(&rich_event()).unwrap(), want);
}

#[test]
fn session_event_to_event_matches_go_for_a_rich_event() {
    let mut wire: Value = serde_json::from_str(GO_RICH_SESSION_EVENT).unwrap();
    wire["name"] = json!("projects/p/locations/l/reasoningEngines/e/sessions/s/events/id");
    let want: Value = serde_json::from_str(GO_RICH_DECODED_EVENT).unwrap();
    assert_eq!(go_json(&session_event_to_event(&wire).unwrap()), want);
}

#[test]
fn event_to_session_event_matches_go_with_a_raw_event() {
    let event = Event {
        id: "evt-raw".into(),
        author: "agent-x".into(),
        invocation_id: "inv-raw".into(),
        timestamp: utc(2026, 3, 4, 5, 6, 7, 123_456_789),
        output: Some(json!(9_007_199_254_740_993_i64)),
        isolation_scope: "scope".into(),
        routes: vec!["r".into()],
        node_info: Some(NodeInfo {
            path: "wf/a".into(),
            message_as_output: true,
            ..Default::default()
        }),
        actions: EventActions {
            state_delta: Some(obj(json!({"count": 3}))),
            artifact_delta: Some(BTreeMap::from([("f".into(), 7)])),
            ..Default::default()
        },
        response: LlmResponse {
            input_transcription: Some(gadk_genai::Transcription {
                text: "in".into(),
                ..Default::default()
            }),
            usage_metadata: Some(GenerateContentResponseUsageMetadata {
                prompt_token_count: 10,
                total_token_count: 12,
                ..Default::default()
            }),
            ..Default::default()
        },
        ..Default::default()
    };
    let want: Value = serde_json::from_str(GO_RAW_SESSION_EVENT).unwrap();
    assert_eq!(event_to_session_event(&event).unwrap(), want);
}

// ---- Envelope details and error texts ----

#[test]
fn session_event_to_event_takes_identity_from_the_envelope() {
    let wire = json!({
        "name": "projects/p/locations/l/reasoningEngines/e/sessions/s/events/77",
        "timestamp": "2025-01-02T03:04:05.5Z",
        "invocationId": "inv-env",
        "author": "env-author",
        "content": {"parts": [{"text": "legacy"}]},
        "rawEvent": {
            "id": "raw-id", "timestamp": "2020-01-01T00:00:00Z", "invocationId": "inv-raw",
            "author": "raw-author", "branch": "b", "output": 3.0,
            "actions": {"artifactDelta": {"f": 2.0}}
        }
    });
    let got = session_event_to_event(&wire).unwrap();
    assert_eq!(got.id, "77");
    assert_eq!(got.timestamp, utc(2025, 1, 2, 3, 4, 5, 500_000_000));
    assert_eq!(got.invocation_id, "inv-env");
    assert_eq!(got.author, "env-author");
    // Everything else comes from rawEvent, not from the legacy fields.
    assert_eq!(got.branch, "b");
    assert_eq!(got.output, Some(json!(3)));
    assert_eq!(
        got.actions.artifact_delta,
        Some(BTreeMap::from([("f".into(), 2)]))
    );
    assert_eq!(got.response.content, None);
}

#[test]
fn session_event_to_event_without_timestamp_is_the_unix_epoch() {
    // Go: (*timestamppb.Timestamp)(nil).AsTime() is 1970-01-01T00:00:00Z.
    let got = session_event_to_event(&json!({"name": "a/b"})).unwrap();
    assert_eq!(got.timestamp, DateTime::UNIX_EPOCH);
    assert_eq!(got.id, "b");
    assert_eq!(got.actions, EventActions::default());
}

#[test]
fn session_event_to_event_error_texts() {
    for (wire, want) in [
        (
            json!({"name": "noslash"}),
            r#"error fetching session events: invalid session name format "noslash": missing separator '/'"#,
        ),
        (
            json!({"name": "a/b/"}),
            r#"error fetching session events: invalid session name "a/b/": empty session ID"#,
        ),
        (
            json!({"name": "a/b", "timestamp": "yesterday"}),
            r#"error fetching session events: invalid google.protobuf.Timestamp value "yesterday""#,
        ),
    ] {
        assert_eq!(session_event_to_event(&wire).unwrap_err().to_string(), want);
    }
    let err = session_event_to_event(&json!({"name": "a/b", "rawEvent": {"actions": 5}}))
        .unwrap_err()
        .to_string();
    assert!(
        err.starts_with("error fetching session events: unmarshaling raw event: "),
        "{err}"
    );
}

#[test]
fn event_to_session_event_wraps_conversion_errors() {
    let event = Event {
        actions: EventActions {
            artifact_delta: Some(BTreeMap::from([("big.bin".into(), -2_147_483_649)])),
            ..Default::default()
        },
        ..Default::default()
    };
    assert_eq!(
        event_to_session_event(&event).unwrap_err().to_string(),
        r#"failed to convert event actions: artifact "big.bin" version -2147483649 does not fit in int32"#
    );
}

#[test]
fn plain_events_send_no_raw_event_and_always_send_event_metadata() {
    let got = event_to_session_event(&Event::default()).unwrap();
    assert_eq!(
        got,
        json!({"timestamp": "0001-01-01T00:00:00Z", "eventMetadata": {}})
    );
}

// ---- Recorded conversations ----

fn fixture(stem: &str) -> Value {
    let path = format!(
        "{}/testdata/replays/{stem}.json",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
}

/// The calls of `method` (`AppendEvent`, `ListEvents`, …) in a fixture, in order.
fn calls(fx: &Value, method: &str) -> Vec<Value> {
    let suffix = format!("/{method}");
    fx["calls"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["method"].as_str().unwrap().ends_with(&suffix))
        .cloned()
        .collect()
}

/// The Rust suite's `simple_event` (Go: `&session.Event{ID, Author: "user", InvocationID}`).
fn simple_event(id: &str, invocation_id: &str) -> Event {
    Event {
        id: id.into(),
        author: "user".into(),
        invocation_id: invocation_id.into(),
        ..Default::default()
    }
}

fn delta_event(key: &str) -> Event {
    Event {
        actions: EventActions {
            state_delta: Some(obj(json!({ key: "v2" }))),
            ..Default::default()
        },
        ..simple_event("event1", "inv1")
    }
}

fn compaction_event() -> Event {
    let start = utc(2026, 3, 4, 5, 6, 7, 123_456_789);
    let end = start + Duration::seconds(5) + Duration::nanoseconds(987);
    Event {
        actions: EventActions {
            compaction: Some(EventCompaction {
                start_timestamp: start,
                end_timestamp: end,
                compacted_content: Some(Content::from_text("summary of earlier turns", "model")),
                excluded_events: vec![
                    EventRef {
                        invocation_id: "inv-1".into(),
                        timestamp: start,
                    },
                    EventRef {
                        invocation_id: "inv-2".into(),
                        timestamp: end,
                    },
                ],
            }),
            ..Default::default()
        },
        ..simple_event("compaction_event", "inv-compaction")
    }
}

fn hole_events() -> Vec<Event> {
    let turn = Event {
        author: "user".into(),
        invocation_id: "inv-hole".into(),
        timestamp: utc(2026, 3, 4, 5, 6, 7, 123_456_789),
        ..Default::default()
    };
    let record = Event {
        author: "user".into(),
        invocation_id: "inv-summary".into(),
        actions: EventActions {
            compaction: Some(EventCompaction {
                start_timestamp: turn.timestamp - Duration::hours(1),
                end_timestamp: turn.timestamp + Duration::hours(1),
                compacted_content: Some(Content::from_text("summary", "model")),
                excluded_events: vec![EventRef {
                    invocation_id: turn.invocation_id.clone(),
                    timestamp: turn.timestamp,
                }],
            }),
            ..Default::default()
        },
        ..Default::default()
    };
    vec![turn, record]
}

fn all_fields_event() -> Event {
    Event {
        long_running_tool_ids: vec!["tool123".into()],
        actions: EventActions {
            state_delta: Some(obj(json!({"user:k2": "v2"}))),
            ..Default::default()
        },
        response: LlmResponse {
            content: Some(Content::from_text("test_text", "user")),
            turn_complete: true,
            partial: false,
            error_code: "error_code".into(),
            error_message: "error_message".into(),
            interrupted: true,
            grounding_metadata: Some(GroundingMetadata {
                web_search_queries: vec!["query1".into()],
                ..Default::default()
            }),
            usage_metadata: Some(GenerateContentResponseUsageMetadata {
                prompt_token_count: 1,
                candidates_token_count: 1,
                total_token_count: 2,
                ..Default::default()
            }),
            citation_metadata: Some(CitationMetadata {
                citations: vec![Citation {
                    title: "test".into(),
                    uri: "google.com".into(),
                    ..Default::default()
                }],
            }),
            custom_metadata: obj(json!({"custom_key": "custom_value"})),
            ..Default::default()
        },
        ..simple_event("event_complete", "inv1")
    }
}

fn bytes_event() -> Event {
    Event {
        response: LlmResponse {
            content: Some(Content::from_bytes(
                Bytes::from_static(b"test_image_data"),
                "image/png",
                "user",
            )),
            ..Default::default()
        },
        ..simple_event("event_with_bytes", "inv1")
    }
}

fn timed(id: &str, invocation_id: &str, secs: i64) -> Event {
    Event {
        timestamp: utc(2025, 1, 1, 0, 0, 0, 0) + Duration::seconds(secs),
        ..simple_event(id, invocation_id)
    }
}

/// Every recorded `AppendEvent`, with the event the shared suite appends in that conversation
/// (the Rust port in `gadk_session::testsuite`, Go's `sessiontestsuite`).
fn recorded_appends() -> Vec<(&'static str, Vec<Event>)> {
    vec![
        (
            "Test_vertexaiService_AppendEvent_a_hole_still_names_its_event_after_a_round_trip",
            hole_events(),
        ),
        (
            "Test_vertexaiService_AppendEvent_a_missing_event_id_is_assigned",
            vec![simple_event("", "inv1"), simple_event("", "inv1")],
        ),
        (
            "Test_vertexaiService_AppendEvent_compaction_record_round_trips",
            vec![compaction_event()],
        ),
        (
            "Test_vertexaiService_AppendEvent_ok",
            vec![simple_event("new_event1", "inv1")],
        ),
        (
            "Test_vertexaiService_AppendEvent_partial_events_are_not_persisted",
            vec![],
        ),
        (
            "Test_vertexaiService_AppendEvent_when_session_not_found_should_fail",
            vec![simple_event("event1", "inv1")],
        ),
        (
            "Test_vertexaiService_AppendEvent_with_all_fields",
            vec![all_fields_event()],
        ),
        (
            "Test_vertexaiService_AppendEvent_with_bytes_content",
            vec![bytes_event()],
        ),
        (
            "Test_vertexaiService_AppendEvent_with_existing_events",
            vec![timed("event1", "inv1", 1), timed("event2", "inv2", 2)],
        ),
        (
            "Test_vertexaiService_Get_get_session_respects_user_id",
            vec![simple_event("event1", "inv1")],
        ),
        (
            "Test_vertexaiService_Get_with_config_filters",
            (1..=5).map(|i| timed(&i.to_string(), "inv1", i)).collect(),
        ),
        (
            "Test_vertexaiService_StateManagement_app_state_is_shared",
            vec![delta_event("app:k2")],
        ),
        (
            "Test_vertexaiService_StateManagement_session_state_is_not_shared",
            vec![delta_event("sk2")],
        ),
        (
            "Test_vertexaiService_StateManagement_user_state_is_user_specific",
            vec![delta_event("user:k2")],
        ),
    ]
}

#[test]
fn event_to_session_event_matches_every_recorded_append() {
    let mut checked = 0;
    for (stem, events) in recorded_appends() {
        let recorded = calls(&fixture(stem), "AppendEvent");
        assert_eq!(recorded.len(), events.len(), "{stem}: AppendEvent count");
        for (i, (call, event)) in recorded.iter().zip(&events).enumerate() {
            let got = event_to_session_event(event).unwrap();
            assert_eq!(got, call["request"]["event"], "{stem}: AppendEvent #{i}");
            checked += 1;
        }
    }
    assert_eq!(checked, 20);
}

#[test]
fn every_recorded_list_events_response_decodes() {
    let dir = format!("{}/testdata/replays", env!("CARGO_MANIFEST_DIR"));
    let mut decoded = 0;
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let stem = path.file_stem().unwrap().to_str().unwrap().to_owned();
        for call in calls(&fixture(&stem), "ListEvents") {
            let Some(events) = call["response"]["sessionEvents"].as_array() else {
                continue;
            };
            for wire in events {
                let got = session_event_to_event(wire).unwrap();
                let name = wire["name"].as_str().unwrap();
                assert_eq!(got.id, name.rsplit('/').next().unwrap(), "{stem}");
                assert_eq!(got.invocation_id, wire["invocationId"], "{stem}");
                assert_eq!(got.author, wire["author"], "{stem}");
                assert_eq!(
                    crate::protojson::format_timestamp(&got.timestamp),
                    wire["timestamp"],
                    "{stem}"
                );
                decoded += 1;
            }
        }
    }
    assert!(decoded > 20, "decoded only {decoded} recorded events");
}

/// The single event of a fixture's last `ListEvents` response, decoded.
fn listed_event(stem: &str) -> Event {
    let list = calls(&fixture(stem), "ListEvents");
    let events = list.last().unwrap()["response"]["sessionEvents"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(events.len(), 1, "{stem}");
    session_event_to_event(&events[0]).unwrap()
}

#[test]
fn recorded_with_all_fields_reads_back_as_appended() {
    // Go's suite compares the whole event, ignoring the server-assigned ID and the
    // citation and usage metadata that no SessionEvent column holds.
    let mut want = all_fields_event();
    want.id = "3590870294731948032".into();
    want.response.citation_metadata = None;
    want.response.usage_metadata = None;
    assert_eq!(
        listed_event("Test_vertexaiService_AppendEvent_with_all_fields"),
        want
    );
}

#[test]
fn recorded_with_bytes_content_reads_back_as_appended() {
    let got = listed_event("Test_vertexaiService_AppendEvent_with_bytes_content");
    assert_eq!(got.response.content, bytes_event().response.content);
    // The recorded response carries `actions: {}`, which Go reads as an empty state delta.
    assert_eq!(got.actions.state_delta, Some(Map::new()));
}

#[test]
fn recorded_compaction_record_reads_back_from_raw_event() {
    let got = listed_event("Test_vertexaiService_AppendEvent_compaction_record_round_trips");
    assert_eq!(
        got.actions.compaction,
        compaction_event().actions.compaction
    );
    assert_eq!(got.id, "1613337729540554752");
    assert_eq!(got.invocation_id, "inv-compaction");
    assert_eq!(got.timestamp, zero_time());
    // rawEvent wins: the recorded `content: {}` envelope field is not read.
    assert_eq!(got.response.content, None);
}

// ---- Malformed protojson scalars fail the whole decode (Go protojson `unmarshalScalar`) ----

fn decode_err(wire: Value) -> String {
    session_event_to_event(&wire).unwrap_err().to_string()
}

#[test]
fn session_event_to_event_rejects_invalid_base64_bytes() {
    let wire = json!({"name": "a/b", "content": {"parts": [{"inlineData": {"data": "***"}}]}});
    assert_eq!(
        decode_err(wire),
        r#"error fetching session events: invalid value for bytes field data: "***""#
    );
    let wire = json!({"name": "a/b", "content": {"parts": [{"thoughtSignature": 7}]}});
    assert_eq!(
        decode_err(wire),
        "error fetching session events: invalid value for bytes field thoughtSignature: 7"
    );
}

#[test]
fn session_event_to_event_rejects_out_of_range_int32() {
    let wire = json!({"name": "a/b", "actions": {"artifactDelta": {"f": 2_147_483_648_i64}}});
    assert_eq!(
        decode_err(wire),
        "error fetching session events: invalid value for int32 field value: 2147483648"
    );
}

#[test]
fn session_event_to_event_rejects_non_integral_int32_string() {
    let wire = json!({"name": "a/b", "eventMetadata": {"groundingMetadata": {
        "groundingSupports": [{"segment": {"partIndex": "3.5"}}]
    }}});
    assert_eq!(
        decode_err(wire),
        r#"error fetching session events: invalid value for int32 field partIndex: "3.5""#
    );
}

#[test]
fn session_event_to_event_rejects_unparsable_float_string() {
    let wire = json!({"name": "a/b", "eventMetadata": {"groundingMetadata": {
        "groundingSupports": [{"confidenceScores": [0.5, "abc"]}]
    }}});
    assert_eq!(
        decode_err(wire),
        r#"error fetching session events: invalid value for float field confidenceScores: "abc""#
    );
}

#[test]
fn session_event_to_event_rejects_non_string_repeated_string_elements() {
    let wire = json!({"name": "a/b", "eventMetadata": {"longRunningToolIds": ["ok", 1]}});
    assert_eq!(
        decode_err(wire),
        "error fetching session events: invalid value for string field longRunningToolIds: 1"
    );
}

#[test]
fn session_event_to_event_accepts_well_formed_edge_scalars() {
    let wire = json!({
        "name": "a/b",
        // URL-alphabet, unpadded: 0xfb 0xff.
        "content": {"parts": [{"inlineData": {"data": "-_8"}}]},
        "actions": {"artifactDelta": {"f": "1e2"}},
        "eventMetadata": {"groundingMetadata": {
            "groundingChunks": [
                {"retrievedContext": {"ragChunk": {"pageSpan": {"firstPage": "1e2"}}}}
            ],
            "groundingSupports": [{"confidenceScores": ["NaN", "-Infinity", "1e2", 0.5]}],
            "retrievalMetadata": {"googleSearchDynamicRetrievalScore": "0.25"}
        }}
    });
    let got = session_event_to_event(&wire).unwrap();
    let content = got.response.content.unwrap();
    let blob = content.parts[0].inline_data.clone().unwrap();
    assert_eq!(blob.data.as_ref(), [0xfb, 0xff]);
    assert_eq!(
        got.actions.artifact_delta,
        Some(BTreeMap::from([("f".into(), 100)]))
    );
    let g = got.response.grounding_metadata.unwrap();
    let rc = g.grounding_chunks[0].retrieved_context.as_ref().unwrap();
    assert_eq!(
        rc.rag_chunk
            .as_ref()
            .unwrap()
            .page_span
            .as_ref()
            .unwrap()
            .first_page,
        100
    );
    assert!(g.grounding_supports[0].confidence_scores[0].is_nan());
}
