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

//! Ports of ADK-Go `server/adkrest/internal/models/event_actions_test.go`. Each Go subtest is a
//! named case; a failure names it.

use std::collections::BTreeMap;

use gadk_genai::{Content, ROLE_MODEL};
use pretty_assertions::assert_eq;
use serde_json::{Map, Value, json};

use crate::rest::models::{Event, EventActions};

fn map(pairs: &[(&str, Value)]) -> Option<Map<String, Value>> {
    Some(
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect(),
    )
}

fn artifacts(pairs: &[(&str, i64)]) -> Option<BTreeMap<String, i64>> {
    Some(pairs.iter().map(|(k, v)| ((*k).to_owned(), *v)).collect())
}

#[test]
fn test_from_session_event_event_actions() {
    let cases: Vec<(&str, gadk_session::EventActions, EventActions)> = vec![
        (
            "all fields populated",
            gadk_session::EventActions {
                state_delta: map(&[("key", json!("value"))]),
                artifact_delta: artifacts(&[("file.txt", 2)]),
                transfer_to_agent: "agent-b".into(),
                escalate: true,
                skip_summarization: true,
                ..Default::default()
            },
            EventActions {
                state_delta: map(&[("key", json!("value"))]),
                artifact_delta: artifacts(&[("file.txt", 2)]),
                transfer_to_agent: "agent-b".into(),
                escalate: true,
                skip_summarization: true,
                ..Default::default()
            },
        ),
        (
            "TransferToAgent only",
            gadk_session::EventActions {
                transfer_to_agent: "orchestrator".into(),
                ..Default::default()
            },
            EventActions {
                transfer_to_agent: "orchestrator".into(),
                ..Default::default()
            },
        ),
        (
            "Escalate only",
            gadk_session::EventActions {
                escalate: true,
                ..Default::default()
            },
            EventActions {
                escalate: true,
                ..Default::default()
            },
        ),
        (
            "SkipSummarization only",
            gadk_session::EventActions {
                skip_summarization: true,
                ..Default::default()
            },
            EventActions {
                skip_summarization: true,
                ..Default::default()
            },
        ),
        (
            "zero value",
            gadk_session::EventActions::default(),
            EventActions::default(),
        ),
    ];
    for (name, actions, want) in cases {
        let src = gadk_session::Event {
            actions,
            ..Default::default()
        };
        let got = Event::from_session_event(&src);
        assert_eq!(
            want, got.actions,
            "{name}: FromSessionEvent().Actions mismatch"
        );
    }
}

#[test]
fn test_to_session_event_event_actions() {
    let cases: Vec<(&str, EventActions, gadk_session::EventActions)> = vec![
        (
            "all fields populated",
            EventActions {
                state_delta: map(&[("k", json!(1))]),
                artifact_delta: artifacts(&[("img.png", 3)]),
                transfer_to_agent: "sub-agent".into(),
                escalate: true,
                skip_summarization: true,
                ..Default::default()
            },
            gadk_session::EventActions {
                state_delta: map(&[("k", json!(1))]),
                artifact_delta: artifacts(&[("img.png", 3)]),
                transfer_to_agent: "sub-agent".into(),
                escalate: true,
                skip_summarization: true,
                ..Default::default()
            },
        ),
        (
            "TransferToAgent only",
            EventActions {
                transfer_to_agent: "root-agent".into(),
                ..Default::default()
            },
            gadk_session::EventActions {
                transfer_to_agent: "root-agent".into(),
                ..Default::default()
            },
        ),
        (
            "Escalate only",
            EventActions {
                escalate: true,
                ..Default::default()
            },
            gadk_session::EventActions {
                escalate: true,
                ..Default::default()
            },
        ),
        (
            "SkipSummarization only",
            EventActions {
                skip_summarization: true,
                ..Default::default()
            },
            gadk_session::EventActions {
                skip_summarization: true,
                ..Default::default()
            },
        ),
        (
            "zero value",
            EventActions::default(),
            gadk_session::EventActions::default(),
        ),
    ];
    for (name, actions, want) in cases {
        let src = Event {
            actions,
            ..Default::default()
        };
        let got = src.to_session_event();
        assert_eq!(
            want, got.actions,
            "{name}: ToSessionEvent().Actions mismatch"
        );
    }
}

#[test]
fn test_from_session_event_round_trip() {
    let original = gadk_session::Event {
        id: "evt-1".into(),
        invocation_id: "inv-1".into(),
        author: "agent".into(),
        response: gadk_model::LlmResponse {
            content: Some(Content::from_text("hello", ROLE_MODEL)),
            ..Default::default()
        },
        actions: gadk_session::EventActions {
            state_delta: map(&[("step", json!("done"))]),
            artifact_delta: artifacts(&[("report.pdf", 1)]),
            transfer_to_agent: "handoff-agent".into(),
            escalate: true,
            skip_summarization: true,
            ..Default::default()
        },
        ..Default::default()
    };

    let got = Event::from_session_event(&original).to_session_event();

    assert_eq!(
        original.actions, got.actions,
        "round-trip EventActions mismatch"
    );
}
