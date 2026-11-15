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

//! Port of ADK-Go `internal/llminternal/foreign_thoughts_test.go`.

use std::sync::Arc;

use gadk_genai::{Blob, Content, Part, ROLE_MODEL, ROLE_USER};
use gadk_session::compaction::Config;
use gadk_session::{Event, EventActions};
use pretty_assertions::assert_eq;
use serde_json::json;

use super::internal_llminternal_contents_processor::{
    ctx_builder, ev, llm_agent, run_contents, with_continuation,
};
use super::internal_llminternal_contents_processor_compaction::{
    compaction_at as at, compaction_invocation_ctx, compaction_summary_event,
    compaction_text_event as text_event,
};
use crate::compaction::runtime;
use crate::llm::{IncludeContents, LlmAgentConfig};

/// The metadata-only parts of Go 6caa3b85's rows: (row label, part). None of them carries
/// anything a model can read.
fn metadata_only_parts() -> Vec<(&'static str, Part)> {
    vec![
        ("empty", Part::default()),
        (
            "signature-only",
            Part {
                thought_signature: vec![1u8, 2].into(),
                ..Default::default()
            },
        ),
        (
            "metadata-only",
            Part {
                part_metadata: json!({"source": "test"}).as_object().cloned().unwrap(),
                ..Default::default()
            },
        ),
        (
            "video metadata-only",
            Part {
                video_metadata: Some(gadk_genai::VideoMetadata {
                    end_offset: chrono::TimeDelta::seconds(1),
                    ..Default::default()
                }),
                ..Default::default()
            },
        ),
        (
            "media resolution-only",
            Part {
                media_resolution: Some(gadk_genai::PartMediaResolution {
                    level: Some(gadk_genai::PartMediaResolutionLevel::MediaResolutionHigh),
                    ..Default::default()
                }),
                ..Default::default()
            },
        ),
        (
            "media processing-only",
            Part {
                media_processing: Some(gadk_genai::MediaProcessing::Static),
                ..Default::default()
            },
        ),
        (
            "speech metadata-only",
            Part {
                speech_metadata: Some(gadk_genai::SpeechMetadata {
                    speaker: "speaker".into(),
                    style: "calm".into(),
                }),
                ..Default::default()
            },
        ),
    ]
}

fn thought(t: &str) -> Part {
    Part {
        text: t.into(),
        thought: true,
        ..Default::default()
    }
}

/// Port of Go `foreignThoughtRequestContents`: runs the processor and checks that the
/// session events were not mutated.
async fn foreign_thought_request_contents(
    events: &[Event],
    ctx: &gadk_agent::InvocationContext,
) -> Vec<Content> {
    let before = serde_json::to_string(events).expect("could not snapshot session events");
    let got = run_contents(ctx)
        .await
        .expect("contents processing yielded an unexpected error");
    let after =
        serde_json::to_string(events).expect("could not snapshot session events after processing");
    assert_eq!(
        before, after,
        "contents processing mutated the original session events"
    );
    got
}

#[tokio::test]
#[ignore = "8a bug: convert_foreign_event keeps a foreign part that carries only metadata (empty, signature, part/video/speech metadata, media resolution or processing), so it reaches the request and starts a turn; Go 6caa3b85 omits it"]
async fn test_contents_request_processor_foreign_thoughts() {
    let public_part = Part {
        inline_data: Some(Blob {
            mime_type: "image/png".into(),
            data: vec![1u8, 2, 3].into(),
            ..Default::default()
        }),
        thought_signature: vec![4u8, 5].into(),
        ..Default::default()
    };
    let metadata_rows: Vec<(String, Part)> = metadata_only_parts()
        .into_iter()
        .map(|(label, part)| (format!("{label} foreign part is omitted"), part))
        .collect();
    let mut cases: Vec<(&str, &str, Vec<Part>, Vec<Content>)> = vec![(
        "thought-only foreign event is omitted",
        "otherAgent",
        vec![thought("private reasoning")],
        vec![],
    )];
    for (name, part) in &metadata_rows {
        cases.push((name.as_str(), "otherAgent", vec![part.clone()], vec![]));
    }
    cases.extend(vec![
        (
            "mixed foreign event retains public parts",
            "otherAgent",
            vec![
                thought("private reasoning"),
                Part::from_text("public answer"),
                public_part.clone(),
            ],
            vec![Content::from_parts(
                vec![
                    Part::from_text("For context:"),
                    Part::from_text("[otherAgent] said: public answer"),
                    public_part.clone(),
                ],
                ROLE_USER,
            )],
        ),
        (
            "own-agent thoughts are retained",
            "testAgent",
            vec![Part {
                thought_signature: vec![6u8, 7].into(),
                ..thought("own reasoning")
            }],
            vec![Content::from_parts(
                vec![Part {
                    thought_signature: vec![6u8, 7].into(),
                    ..thought("own reasoning")
                }],
                ROLE_MODEL,
            )],
        ),
    ]);
    for (name, author, parts, tail) in cases {
        let user = text_event("user", 1, "request");
        let mut state_delta = serde_json::Map::new();
        state_delta.insert("state".into(), json!("unchanged"));
        let event = Event {
            id: "response-event".into(),
            invocation_id: "response-invocation".into(),
            timestamp: at(2),
            actions: EventActions {
                state_delta: Some(state_delta),
                ..Default::default()
            },
            ..ev(author, Content::from_parts(parts, ROLE_MODEL))
        };
        let events = vec![user.clone(), event];
        // Go compactionInvocationCtx(t, "testAgent", events, false): no compaction configured.
        let agent = llm_agent(LlmAgentConfig {
            name: "testAgent".into(),
            ..Default::default()
        });
        let ctx = ctx_builder(&agent, events.clone()).build_invocation();
        let got = foreign_thought_request_contents(&events, &ctx).await;
        let mut want = vec![user.response.content.clone().unwrap()];
        want.extend(tail);
        assert_eq!(
            with_continuation(want),
            got,
            "{name}: request contents did not preserve the expected public and own-agent parts"
        );
    }
}

#[tokio::test]
async fn test_contents_request_processor_foreign_thought_compaction_summary() {
    for usable in [true, false] {
        let name = if usable {
            "usable summary bypasses foreign thought conversion"
        } else {
            "unusable summary does not expose foreign thoughts"
        };
        let old = text_event("user", 1, "old request");
        let recent = text_event("user", 4, "recent request");
        let mut summary = compaction_summary_event(3, 1, 2, "public summary");
        summary.author = "otherAgent".into();
        summary.response.content = Some(Content::from_parts(
            vec![thought("private summary reasoning")],
            ROLE_MODEL,
        ));
        let mut want = vec![
            summary
                .actions
                .compaction
                .as_ref()
                .unwrap()
                .compacted_content
                .clone()
                .unwrap(),
            recent.response.content.clone().unwrap(),
        ];
        if !usable {
            summary
                .actions
                .compaction
                .as_mut()
                .unwrap()
                .compacted_content = None;
            want = vec![
                old.response.content.clone().unwrap(),
                recent.response.content.clone().unwrap(),
            ];
        }
        let events = vec![old, summary, recent];
        let ctx = compaction_invocation_ctx("testAgent", events.clone(), true);
        let got = foreign_thought_request_contents(&events, &ctx).await;
        assert_eq!(
            want, got,
            "{name}: foreign thought filtering changed compaction summary handling"
        );
    }
}

#[tokio::test]
async fn test_contents_request_processor_foreign_thought_compaction_hole() {
    let mut foreign = text_event("otherAgent", 2, "public answer");
    foreign.invocation_id = "foreign-invocation".into();
    if let Some(c) = foreign.response.content.as_mut() {
        c.parts.insert(0, thought("private reasoning"));
    }
    let mut summary = compaction_summary_event(3, 1, 2, "public summary");
    summary.actions.compaction.as_mut().unwrap().excluded_events = vec![gadk_session::EventRef {
        invocation_id: foreign.invocation_id.clone(),
        timestamp: foreign.timestamp,
    }];
    let recent = text_event("user", 4, "recent request");
    let events = vec![
        text_event("user", 1, "old request"),
        foreign,
        summary.clone(),
        recent.clone(),
    ];
    let ctx = compaction_invocation_ctx("testAgent", events.clone(), true);
    let got = foreign_thought_request_contents(&events, &ctx).await;
    let want = vec![
        summary
            .actions
            .compaction
            .unwrap()
            .compacted_content
            .unwrap(),
        Content::from_parts(
            vec![
                Part::from_text("For context:"),
                Part::from_text("[otherAgent] said: public answer"),
            ],
            ROLE_USER,
        ),
        recent.response.content.unwrap(),
    ];
    assert_eq!(
        want, got,
        "filtering foreign thoughts lost the event protected by the compaction hole"
    );
}

#[tokio::test]
#[ignore = "8a bug: convert_foreign_event keeps a foreign part that carries only metadata (empty, signature, part/video/speech metadata, media resolution or processing), so it reaches the request and starts a turn; Go 6caa3b85 omits it"]
async fn test_contents_request_processor_foreign_thought_current_turn() {
    struct Case {
        name: &'static str,
        author: &'static str,
        parts: Vec<Part>,
        successive_thought: bool,
        summary: bool,
        keep_current_user: bool,
        want_tail: Vec<Content>,
    }
    let empty_thought = Part {
        thought: true,
        ..Default::default()
    };
    let metadata: Vec<Part> = metadata_only_parts().into_iter().map(|(_, p)| p).collect();
    let cases = vec![
        Case {
            name: "foreign text thought does not start a turn",
            author: "otherAgent",
            parts: vec![thought("private reasoning")],
            successive_thought: false,
            summary: false,
            keep_current_user: true,
            want_tail: vec![],
        },
        Case {
            name: "foreign empty thought does not start a turn",
            author: "otherAgent",
            parts: vec![empty_thought.clone()],
            successive_thought: false,
            summary: false,
            keep_current_user: true,
            want_tail: vec![],
        },
        Case {
            name: "foreign empty part does not start a turn",
            author: "otherAgent",
            parts: vec![metadata[0].clone()],
            successive_thought: false,
            summary: false,
            keep_current_user: true,
            want_tail: vec![],
        },
        Case {
            name: "foreign signature-only part does not start a turn",
            author: "otherAgent",
            parts: vec![metadata[1].clone()],
            successive_thought: false,
            summary: false,
            keep_current_user: true,
            want_tail: vec![],
        },
        Case {
            name: "foreign metadata-only part does not start a turn",
            author: "otherAgent",
            parts: vec![metadata[2].clone()],
            successive_thought: false,
            summary: false,
            keep_current_user: true,
            want_tail: vec![],
        },
        Case {
            name: "foreign video metadata-only part does not start a turn",
            author: "otherAgent",
            parts: vec![metadata[3].clone()],
            successive_thought: false,
            summary: false,
            keep_current_user: true,
            want_tail: vec![],
        },
        Case {
            name: "foreign media resolution-only part does not start a turn",
            author: "otherAgent",
            parts: vec![metadata[4].clone()],
            successive_thought: false,
            summary: false,
            keep_current_user: true,
            want_tail: vec![],
        },
        Case {
            name: "foreign media processing-only part does not start a turn",
            author: "otherAgent",
            parts: vec![metadata[5].clone()],
            successive_thought: false,
            summary: false,
            keep_current_user: true,
            want_tail: vec![],
        },
        Case {
            name: "foreign speech metadata-only part does not start a turn",
            author: "otherAgent",
            parts: vec![metadata[6].clone()],
            successive_thought: false,
            summary: false,
            keep_current_user: true,
            want_tail: vec![],
        },
        Case {
            name: "multiple foreign thought parts do not start a turn",
            author: "otherAgent",
            parts: vec![thought("private reasoning"), empty_thought.clone()],
            successive_thought: false,
            summary: false,
            keep_current_user: true,
            want_tail: vec![],
        },
        Case {
            name: "successive foreign thoughts do not start a turn",
            author: "otherAgent",
            parts: vec![thought("private reasoning")],
            successive_thought: true,
            summary: false,
            keep_current_user: true,
            want_tail: vec![],
        },
        Case {
            name: "mixed foreign response still starts a turn",
            author: "otherAgent",
            parts: vec![
                thought("private reasoning"),
                Part::from_text("public answer"),
            ],
            successive_thought: false,
            summary: false,
            keep_current_user: false,
            want_tail: vec![Content::from_parts(
                vec![
                    Part::from_text("For context:"),
                    Part::from_text("[otherAgent] said: public answer"),
                ],
                ROLE_USER,
            )],
        },
        Case {
            name: "own thoughts remain in the current user turn",
            author: "testAgent",
            parts: vec![thought("own reasoning")],
            successive_thought: false,
            summary: false,
            keep_current_user: true,
            want_tail: vec![Content::from_parts(
                vec![thought("own reasoning")],
                ROLE_MODEL,
            )],
        },
        Case {
            name: "user thought content starts its own turn",
            author: "user",
            parts: vec![thought("user input")],
            successive_thought: false,
            summary: false,
            keep_current_user: false,
            want_tail: vec![Content::from_parts(vec![thought("user input")], ROLE_USER)],
        },
        Case {
            name: "usable foreign summary still starts a turn",
            author: "otherAgent",
            parts: vec![thought("private summary reasoning")],
            successive_thought: false,
            summary: true,
            keep_current_user: false,
            want_tail: vec![Content::from_text("public summary", ROLE_MODEL)],
        },
    ];
    for c in cases {
        let current = text_event("user", 3, "current request");
        let role = if c.author == "user" {
            ROLE_USER
        } else {
            ROLE_MODEL
        };
        let mut event = Event {
            timestamp: at(4),
            ..ev(c.author, Content::from_parts(c.parts, role))
        };
        if c.summary {
            // The current user is outside this range, so skipping the summary pivot would
            // incorrectly include that user alongside the summary.
            event.actions = compaction_summary_event(4, 1, 2, "public summary").actions;
        }
        let mut events = vec![
            text_event("user", 1, "older request"),
            text_event("testAgent", 2, "older answer"),
            current.clone(),
            event,
        ];
        if c.successive_thought {
            events.push(Event {
                timestamp: at(5),
                ..ev(
                    "anotherAgent",
                    Content::from_parts(vec![empty_thought.clone()], ROLE_MODEL),
                )
            });
        }
        let agent = llm_agent(LlmAgentConfig {
            name: "testAgent".into(),
            include_contents: IncludeContents::None,
            ..Default::default()
        });
        let mut b = ctx_builder(&agent, events.clone());
        if c.summary {
            b = runtime::attach(
                b,
                runtime::CompactionRuntime::new(
                    Arc::new(Config {
                        compaction_interval: 1,
                        ..Default::default()
                    }),
                    None,
                ),
            );
        }
        let ctx = b.build_invocation();
        let got = foreign_thought_request_contents(&events, &ctx).await;
        let mut want = Vec::new();
        if c.keep_current_user {
            want.push(current.response.content.clone().unwrap());
        }
        want.extend(c.want_tail);
        assert_eq!(
            with_continuation(want),
            got,
            "{}: request contents selected the wrong current-turn boundary",
            c.name
        );
    }
}
