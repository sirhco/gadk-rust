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

//! Ports of ADK-Go `server/adka2a/v2/metadata_test.go`.

#![allow(clippy::unwrap_used)]

use std::collections::HashMap;

use gadk_genai::{
    GenerateContentResponseUsageMetadata, GroundingMetadata, GroundingMetadataSourceFlaggingURI,
    MediaModality, ModalityTokenCount, RetrievalMetadata,
};
use gadk_model::LlmResponse;
use gadk_session::{Event, EventActions};
use pretty_assertions::assert_eq;
use serde_json::{Map, Value, json};

use crate::convert::{
    METADATA_CUSTOM_META_KEY, METADATA_ERROR_CODE_KEY, METADATA_ESCALATE_KEY,
    METADATA_GROUNDING_KEY, METADATA_TRANSFER_TO_AGENT_KEY, METADATA_USAGE_KEY, process_a2a_meta,
    set_actions_meta, to_event_meta,
};

fn obj(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        other => panic!("not an object: {other}"),
    }
}

fn meta(v: Value) -> HashMap<String, Value> {
    obj(v).into_iter().collect()
}

fn response_event(response: LlmResponse) -> Event {
    Event {
        response,
        ..Default::default()
    }
}

/// Go compares `map[string]any` holding JSON-decoded numbers (`float64(1)`); serde_json keeps
/// integers, so the expected numbers are written as integers (identical on the wire, D5).
#[test]
fn test_metadata_two_way_conversion() {
    struct Case {
        name: &'static str,
        event: Event,
        a2a_meta: HashMap<String, Value>,
        // Overrides `event` for the process_a2a_meta direction; needed only when
        // transfer_to_agent is set, since that is never restored from peer metadata.
        want_after_process_a2a_meta: Option<Event>,
    }

    let composite_response = LlmResponse {
        grounding_metadata: Some(GroundingMetadata {
            source_flagging_uris: vec![GroundingMetadataSourceFlaggingURI {
                source_id: "id1".into(),
                ..Default::default()
            }],
            ..Default::default()
        }),
        usage_metadata: Some(GenerateContentResponseUsageMetadata {
            candidates_token_count: 12,
            thoughts_token_count: 42,
            ..Default::default()
        }),
        custom_metadata: obj(json!({"nested": {"key": "value"}})),
        ..Default::default()
    };

    let cases = vec![
        Case {
            name: "error code",
            event: response_event(LlmResponse {
                error_code: "102".into(),
                ..Default::default()
            }),
            a2a_meta: meta(json!({ (METADATA_ERROR_CODE_KEY): "102" })),
            want_after_process_a2a_meta: None,
        },
        Case {
            name: "custom metadata",
            event: response_event(LlmResponse {
                custom_metadata: obj(json!({"nested": {"key": "value"}})),
                ..Default::default()
            }),
            a2a_meta: meta(json!({ (METADATA_CUSTOM_META_KEY): {"nested": {"key": "value"}} })),
            want_after_process_a2a_meta: None,
        },
        Case {
            name: "usage",
            event: response_event(LlmResponse {
                usage_metadata: Some(GenerateContentResponseUsageMetadata {
                    cache_tokens_details: vec![ModalityTokenCount {
                        modality: Some(MediaModality::Unknown("text".into())),
                        token_count: 1,
                    }],
                    prompt_token_count: 1,
                    tool_use_prompt_token_count: 1,
                    ..Default::default()
                }),
                ..Default::default()
            }),
            a2a_meta: meta(json!({ (METADATA_USAGE_KEY): {
                "cacheTokensDetails": [{"modality": "text", "tokenCount": 1}],
                "promptTokenCount": 1,
                "toolUsePromptTokenCount": 1,
            }})),
            want_after_process_a2a_meta: None,
        },
        Case {
            name: "grounding",
            event: response_event(LlmResponse {
                grounding_metadata: Some(GroundingMetadata {
                    retrieval_queries: vec!["hello".into()],
                    retrieval_metadata: Some(RetrievalMetadata {
                        google_search_dynamic_retrieval_score: 23.3,
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            a2a_meta: meta(json!({ (METADATA_GROUNDING_KEY): {
                "retrievalQueries": ["hello"],
                "retrievalMetadata": {"googleSearchDynamicRetrievalScore": 23.3},
            }})),
            want_after_process_a2a_meta: None,
        },
        Case {
            name: "actions",
            event: Event {
                actions: EventActions {
                    transfer_to_agent: "another".into(),
                    escalate: true,
                    ..Default::default()
                },
                ..Default::default()
            },
            a2a_meta: meta(json!({
                (METADATA_TRANSFER_TO_AGENT_KEY): "another",
                (METADATA_ESCALATE_KEY): true,
            })),
            // process_a2a_meta never restores transfer_to_agent from peer metadata.
            want_after_process_a2a_meta: Some(Event {
                actions: EventActions {
                    escalate: true,
                    ..Default::default()
                },
                ..Default::default()
            }),
        },
        Case {
            name: "composite",
            event: Event {
                response: composite_response.clone(),
                actions: EventActions {
                    transfer_to_agent: "another".into(),
                    escalate: true,
                    ..Default::default()
                },
                ..Default::default()
            },
            a2a_meta: meta(json!({
                (METADATA_GROUNDING_KEY): {"sourceFlaggingUris": [{"sourceId": "id1"}]},
                (METADATA_USAGE_KEY): {"candidatesTokenCount": 12, "thoughtsTokenCount": 42},
                (METADATA_CUSTOM_META_KEY): {"nested": {"key": "value"}},
                (METADATA_TRANSFER_TO_AGENT_KEY): "another",
                (METADATA_ESCALATE_KEY): true,
            })),
            // process_a2a_meta never restores transfer_to_agent from peer metadata;
            // everything else round-trips.
            want_after_process_a2a_meta: Some(Event {
                response: composite_response,
                actions: EventActions {
                    escalate: true,
                    ..Default::default()
                },
                ..Default::default()
            }),
        },
    ];

    for tc in cases {
        let mut got_meta = to_event_meta(&HashMap::new(), &tc.event).unwrap();
        set_actions_meta(&mut got_meta, &tc.event.actions);
        assert_eq!(got_meta, tc.a2a_meta, "{}: to_event_meta", tc.name);

        let mut event = Event::default();
        // process_a2a_meta never restores transfer_to_agent from peer metadata
        // (transfer_to_agent_from_meta is how a caller restores it explicitly, after making its
        // own trust decision about the remote peer).
        let update = a2a::StreamResponse::StatusUpdate(a2a::TaskStatusUpdateEvent {
            task_id: String::new(),
            context_id: String::new(),
            status: a2a::TaskStatus {
                state: a2a::TaskState::Unspecified,
                message: None,
                timestamp: None,
            },
            metadata: Some(got_meta),
        });
        process_a2a_meta(&update, &mut event).unwrap();
        let want = tc.want_after_process_a2a_meta.unwrap_or(tc.event);
        assert_eq!(event, want, "{}: process_a2a_meta", tc.name);
    }
}
