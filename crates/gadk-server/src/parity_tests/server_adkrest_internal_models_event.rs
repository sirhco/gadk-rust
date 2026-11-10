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

//! Ports of ADK-Go `server/adkrest/internal/models/event_test.go`.

use std::collections::BTreeMap;

use chrono::DateTime;
use gadk_genai::Content;
use gadk_session::{EventActions, EventCompaction, NodeInfo, RequestInput, ToolConfirmation};
use serde_json::json;

use crate::rest::models;

#[test]
fn test_event_round_trip_preserves_workflow_fields() {
    let ev = gadk_session::Event {
        id: "e1".into(),
        author: "agent".into(),
        isolation_scope: "scope-1".into(),
        routes: vec!["next-node".into(), "fallback".into()],
        requested_input: Some(RequestInput {
            interrupt_id: "approve".into(),
            message: "ok?".into(),
            ..Default::default()
        }),
        node_info: Some(NodeInfo {
            path: "root/child".into(),
            message_as_output: true,
            ..Default::default()
        }),
        output: Some(json!("hello")),
        actions: EventActions {
            requested_tool_confirmations: BTreeMap::from([(
                "call-1".to_owned(),
                ToolConfirmation {
                    hint: "please confirm".into(),
                    ..Default::default()
                },
            )]),
            ..Default::default()
        },
        ..Default::default()
    };

    let back = models::Event::from_session_event(&ev).to_session_event();

    assert_eq!(back.isolation_scope, ev.isolation_scope, "IsolationScope");
    assert!(
        back.routes.len() == ev.routes.len() && back.routes[0] == "next-node",
        "Routes = {:?}, want {:?}",
        back.routes,
        ev.routes
    );
    assert!(
        back.requested_input
            .as_ref()
            .is_some_and(|r| r.interrupt_id == "approve"),
        "RequestedInput = {:?}, want InterruptID=approve",
        back.requested_input
    );
    assert!(
        back.node_info
            .as_ref()
            .is_some_and(|n| n.path == "root/child"),
        "NodeInfo = {:?}, want Path=root/child",
        back.node_info
    );
    assert!(
        back.actions
            .requested_tool_confirmations
            .get("call-1")
            .is_some_and(|c| c.hint == "please confirm"),
        "RequestedToolConfirmations = {:?}, want call-1 hint",
        back.actions.requested_tool_confirmations
    );
}

#[test]
fn test_compaction_is_read_only_over_rest() {
    let record = EventCompaction {
        start_timestamp: DateTime::from_timestamp(1, 0).unwrap(),
        end_timestamp: DateTime::from_timestamp(9, 0).unwrap(),
        compacted_content: Some(Content::from_text("planted", "model")),
        excluded_events: vec![],
    };

    // Inbound: a client-supplied record must not survive.
    let inbound = models::Event {
        actions: models::EventActions {
            compaction: Some(record.clone()),
            ..Default::default()
        },
        ..Default::default()
    };
    assert!(
        inbound.to_session_event().actions.compaction.is_none(),
        "ToSessionEvent kept a client-supplied compaction record; it must be dropped"
    );

    // Outbound: a stored record must be returned.
    let stored = gadk_session::Event {
        actions: EventActions {
            compaction: Some(record.clone()),
            ..Default::default()
        },
        ..Default::default()
    };
    let out = models::Event::from_session_event(&stored);
    let Some(c) = out.actions.compaction else {
        panic!("FromSessionEvent dropped the stored compaction record; reads should show it");
    };
    assert_eq!(c.end_timestamp, record.end_timestamp, "EndTimestamp");
}
