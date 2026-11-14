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

//! Part of the port of ADK-Go `plugin/agentanalytics/events_test.go` (owner: Task 11): the 14 Go
//! tests from `TestSessionEvents_EmitRichLifecycleTypes` to
//! `TestSensitiveKeys_StandardRedactionInCompactionAndA2A` in the Go file's order that need no
//! runner (the exact list is in the 7b-3a plan's contracts, Appendix A).
//!
//! Go's `setupTestPlugin` / `setupTestPluginWithConfig` are
//! [`setup_test_plugin`] / [`new_test_plugin_with_config`]; Go's `mCtx` is the plugin's
//! [`MockCtx`]; a Go `t.Run` subtest is a block (or a loop body) of one Rust test, each with a
//! plugin of its own as in Go.

use std::sync::Arc;

use chrono::{TimeZone, Utc};
use gadk_agent::{REQUEST_CONFIRMATION_FUNCTION_CALL_NAME, Tool, ToolError};
use gadk_engine::llm::LlmAgentConfig;
use gadk_engine::tool::AgentTool;
use gadk_engine::tool::function::FunctionToolConfig;
use gadk_engine::workflow::WORKFLOW_INPUT_FUNCTION_CALL_NAME;
use gadk_genai::{Content, FunctionCall, FunctionResponse, Part};
use gadk_session::{Event, EventCompaction, NodeInfo, RequestInput, ToolConfirmation};
use serde_json::{Map, Value, json};

use super::{
    FakeTool, after_run, after_tool, before_run, before_tool, check_json, check_json_at, confirmed,
    function_tool, llm, local_agent, obj, on_event, on_tool_error, on_user_message, remote_agent,
    text_content,
};
use crate::config::Config;
use crate::event_types::*;
use crate::events::{
    HITL_CREDENTIAL_FUNCTION_CALL_NAME, TOOL_ORIGIN_A2A, TOOL_ORIGIN_LOCAL, TOOL_ORIGIN_UNKNOWN,
    extract_event_compaction,
};
use crate::parity_tests::plugin_agentanalytics_bigquery_agent_analytics_plugin::support::{
    MockCtx, TestPlugin, event_types, json_object_column, new_test_plugin_with_config,
    only_row_of_type, rows_of_type, setup_test_plugin, str_col, until_event_type,
};
use crate::plugin::{ADK_ENVELOPE_SCHEMA_VERSION, AnalyticsPlugin, EventData};
use crate::redaction::COLUMN_LATENCY_MS;
use crate::row::{Row, RowValue};
use crate::trace::{Identity, TraceManager};

/// Go `&session.Event{ID: id, Author: author}`.
fn event(id: &str, author: &str) -> Event {
    Event {
        id: id.into(),
        author: author.into(),
        ..Event::default()
    }
}

/// A part holding `call` (Go `{FunctionCall: &genai.FunctionCall{…}}`).
fn call_part(id: &str, name: &str, args: Value) -> Part {
    Part {
        function_call: Some(FunctionCall {
            id: id.into(),
            name: name.into(),
            args: obj(args),
            ..FunctionCall::default()
        }),
        ..Part::default()
    }
}

/// A part holding a function response (Go `{FunctionResponse: &genai.FunctionResponse{…}}`).
fn response_part(id: &str, name: &str, response: Value) -> Part {
    Part {
        function_response: Some(FunctionResponse {
            id: id.into(),
            name: name.into(),
            response: obj(response),
            ..FunctionResponse::default()
        }),
        ..Part::default()
    }
}

/// A text part (Go `{Text: text, Thought: thought}`).
fn text_part_of(text: &str, thought: bool) -> Part {
    Part {
        text: text.into(),
        thought,
        ..Part::default()
    }
}

/// Go `&genai.Content{Role: role, Parts: parts}`.
fn content(role: &str, parts: Vec<Part>) -> Content {
    Content {
        role: role.into(),
        parts,
    }
}

/// Go `textPart(text, index)`: the `content_parts` entry of a text part.
fn text_part(text: &str, index: i64) -> Row {
    Row::from([
        ("mime_type".to_owned(), RowValue::from("text/plain")),
        ("text".to_owned(), RowValue::from(text)),
        ("part_index".to_owned(), RowValue::Int(index)),
        ("storage_mode".to_owned(), RowValue::from("INLINE")),
    ])
}

/// The `is_truncated` column (Go `row["is_truncated"]`).
fn is_truncated(row: &Row) -> Option<bool> {
    match row.get("is_truncated") {
        Some(RowValue::Bool(b)) => Some(*b),
        _ => None,
    }
}

/// Go `json.Marshal(row)` for the leak checks: the row's columns as text.
fn row_text(row: &Row) -> String {
    format!("{row:?}")
}

/// Go `p.AfterRunCallback()(mCtx)` then `collectRows(t, requests,
/// untilEventType(EventInvocationCompleted))`.
async fn finish(tp: &TestPlugin) -> Vec<Row> {
    after_run(tp, &tp.ctx).await;
    tp.collect_rows(until_event_type(EVENT_INVOCATION_COMPLETED))
        .await
}

/// Go `setupTestPluginWithConfig(t, mutate)`.
async fn plugin_with(mutate: impl FnOnce(&mut Config)) -> TestPlugin {
    new_test_plugin_with_config(mutate).await
}

// parity: adapted: Go's typed, struct and cyclic metadata / args are their JSON encodings
#[tokio::test]
async fn test_session_events_emit_rich_lifecycle_types() {
    // STATE_DELTA scrubs temp: keys case-sensitively and writes attributes.state_delta with
    // NULL content.
    {
        let tp = setup_test_plugin().await;
        // Temp-only delta ("temp:" prefix) must be suppressed completely.
        let mut ev = event("ev-temp-only", "test-agent");
        ev.actions.state_delta = Some(obj(json!({
            "temp:scratch": "ephemeral",
            "temp:token": "secret",
        })));
        on_event(&tp, &tp.ctx, ev).await;
        // Mixed delta must strip lowercase "temp:" keys and keep durable keys (including
        // uppercase "TEMP:" keys, matching ADK Go's case-sensitive KeyPrefixTemp).
        let mut ev = event("ev-state-1", "test-agent");
        ev.actions.state_delta = Some(obj(json!({
            "temp:scratch": "ephemeral",
            "TEMP:durable": "kept",
            "counter": 42,
            "mode": "active",
        })));
        on_event(&tp, &tp.ctx, ev).await;
        let rows = finish(&tp).await;
        assert_eq!(
            event_types(&rows),
            [EVENT_STATE_DELTA, EVENT_INVOCATION_COMPLETED],
            "event types"
        );
        let row = only_row_of_type(&rows, EVENT_STATE_DELTA);
        assert!(
            !row.contains_key("content"),
            "STATE_DELTA content = {:?}, want absent/NULL",
            row.get("content")
        );
        let attrs = json_object_column(&row, "attributes");
        // "TEMP:durable" is retained by scrubTempStateDelta (case-sensitive "temp:") and its
        // value is redacted by smartTruncate's sensitive-key check.
        check_json_at(
            &attrs,
            r#"{"TEMP:durable":"[REDACTED]","counter":42,"mode":"active"}"#,
            &["state_delta"],
        );
        check_json_at(&attrs, r#""ev-state-1""#, &["adk", "source_event_id"]);
    }

    // AGENT_TRANSFER
    {
        let tp = setup_test_plugin().await;
        let mut ev = event("ev-transfer-1", "router-agent");
        ev.actions.transfer_to_agent = "specialist-agent".into();
        on_event(&tp, &tp.ctx, ev).await;
        let rows = finish(&tp).await;
        let row = only_row_of_type(&rows, EVENT_AGENT_TRANSFER);
        check_json(
            "AGENT_TRANSFER content",
            &json_object_column(&row, "content"),
            r#"{"from_agent":"router-agent","to_agent":"specialist-agent"}"#,
        );
    }

    // EVENT_COMPACTION fractional epoch timestamps, formatResponseContent, and
    // Actions.Compaction extraction.
    //
    // Adapted: gadk custom metadata is JSON, so Go's struct values under `adk_compaction` are
    // their JSON encodings (Go field names, RFC 3339 times, the content as JSON); gadk's
    // `EventActions` always has `compaction` (Go's reflection probe finds it too).
    {
        let tp = plugin_with(|c| {
            c.max_content_len = 40;
            c.custom_metadata_allowlist = vec!["adk_compaction".into()];
        })
        .await;
        let start_ts = Utc.timestamp_opt(1_700_000_000, 500_000_000).unwrap();
        let end_ts = Utc.timestamp_opt(1_700_000_300, 250_000_000).unwrap();

        // 1. Application metadata under "compaction" (or non-compaction maps/structs) must NOT
        // emit EVENT_COMPACTION.
        let mut ev = event("ev-app-meta-ignored", "test-agent");
        ev.response.custom_metadata = obj(json!({
            "compaction": {"policy": "aggressive", "max_turns": 20},
            "adk_compaction": {"unrelated": "ignored"},
        }));
        on_event(&tp, &tp.ctx, ev).await;
        let mut ev = event("ev-app-meta-struct-ignored", "test-agent");
        ev.response.custom_metadata = obj(json!({"adk_compaction": {"Unrelated": "noop"}}));
        on_event(&tp, &tp.ctx, ev).await;
        let mut ev = event("ev-app-meta-ptr-struct-ignored", "test-agent");
        ev.response.custom_metadata = obj(json!({"adk_compaction": {"Unrelated": "noop"}}));
        on_event(&tp, &tp.ctx, ev).await;

        // 2. Real session.EventActions.Compaction via OnEventCallback.
        let mut ev = event("ev-actions-compact", "test-agent");
        ev.actions.compaction = Some(EventCompaction {
            start_timestamp: start_ts,
            end_timestamp: end_ts,
            compacted_content: Some(text_content("model", "short summary")),
            excluded_events: Vec::new(),
        });
        on_event(&tp, &tp.ctx, ev).await;

        // 3. Fallback via CustomMetadata["adk_compaction"] with truncation.
        let mut ev = event("ev-compact-1", "test-agent");
        ev.response.custom_metadata = obj(json!({"adk_compaction": {
            "StartTimestamp": "2023-11-14T22:13:20.5Z",
            "EndTimestamp": "2023-11-14T22:18:20.25Z",
            "CompactedContent": {
                "role": "model",
                "parts": [{"text": "this compacted summary is longer than 40 chars"}],
            },
        }}));
        on_event(&tp, &tp.ctx, ev).await;
        let rows = finish(&tp).await;
        let comp_rows = rows_of_type(&rows, EVENT_EVENT_COMPACTION);
        assert_eq!(comp_rows.len(), 2, "EVENT_COMPACTION rows");
        check_json(
            "Actions.Compaction EVENT_COMPACTION content",
            &json_object_column(&comp_rows[0], "content"),
            r#"{"start_timestamp":1700000000.5,"end_timestamp":1700000300.25,"compacted_content":"text: 'short summary'"}"#,
        );
        assert_eq!(
            is_truncated(&comp_rows[0]),
            Some(false),
            "Actions.Compaction is_truncated"
        );
        let meta_row = &comp_rows[comp_rows.len() - 1];
        check_json(
            "adk_compaction EVENT_COMPACTION content",
            &json_object_column(meta_row, "content"),
            r#"{"start_timestamp":1700000000.5,"end_timestamp":1700000300.25,"compacted_content":"text: 'this compacted summary is longer ...[TRUNCATED]"}"#,
        );
        assert_eq!(is_truncated(meta_row), Some(true), "is_truncated");

        // Direct unit test for extractActionsCompaction on an Actions value with Compaction
        // (gadk: `extract_event_compaction` of an event with `actions.compaction`).
        let mut ev = Event::default();
        ev.actions.compaction = Some(EventCompaction {
            start_timestamp: start_ts,
            end_timestamp: end_ts,
            compacted_content: Some(text_content("model", "short summary")),
            excluded_events: Vec::new(),
        });
        let (payload, trunc, ok) = extract_event_compaction(&ev, None, 100, true);
        assert!(
            ok && !trunc,
            "extractActionsCompaction() = ({payload:?}, {trunc}, {ok}), want (payload, false, true)"
        );
        assert_eq!(
            payload.unwrap()["compacted_content"],
            json!("text: 'short summary'"),
            "extractActionsCompaction compacted_content"
        );
    }

    // AGENT_STATE_CHECKPOINT keys and terminal-only checkpoints
    let cases: [(&str, Value, Option<&str>); 5] = [
        (
            "adk_agent_state with end_of_agent",
            json!({"adk_agent_state": {"step": 2, "cursor": "abc"}, "end_of_agent": true}),
            Some(r#"{"agent_state":{"step":2,"cursor":"abc"},"end_of_agent":true}"#),
        ),
        (
            "alternate agent_state key",
            json!({"agent_state": {"step": 3}, "end_of_agent": false}),
            Some(r#"{"agent_state":{"step":3},"end_of_agent":false}"#),
        ),
        (
            "terminal checkpoint with nil state payload",
            json!({"agent_state": null, "end_of_agent": true}),
            Some(r#"{"agent_state":null,"end_of_agent":true}"#),
        ),
        (
            "terminal checkpoint with adk_end_of_agent only",
            json!({"adk_end_of_agent": true}),
            Some(r#"{"agent_state":null,"end_of_agent":true}"#),
        ),
        (
            "nil state and non-terminal does not emit",
            json!({"agent_state": null, "end_of_agent": false}),
            None,
        ),
    ];
    for (name, meta, want) in cases {
        let tp = plugin_with(|c| {
            c.custom_metadata_allowlist = vec![
                "agent_state".into(),
                "adk_agent_state".into(),
                "adk_end_of_agent".into(),
                "end_of_agent".into(),
            ];
        })
        .await;
        let mut ev = event("ev-ckpt", "workflow-agent");
        ev.response.custom_metadata = obj(meta);
        on_event(&tp, &tp.ctx, ev).await;
        let rows = finish(&tp).await;
        let ckpt_rows = rows_of_type(&rows, EVENT_AGENT_STATE_CHECKPOINT);
        let Some(want) = want else {
            assert!(ckpt_rows.is_empty(), "{name}: AGENT_STATE_CHECKPOINT rows");
            continue;
        };
        let row = only_row_of_type(&rows, EVENT_AGENT_STATE_CHECKPOINT);
        check_json(
            &format!("{name}: AGENT_STATE_CHECKPOINT content"),
            &json_object_column(&row, "content"),
            want,
        );
    }

    // HITL requests, credential scrubbing (camelCase+snake_case), TOOL_PAUSED, and HITL
    // completions
    {
        let tp = setup_test_plugin().await;
        // 1. Session event with LongRunningToolIDs matching HITL credential (with camelCase &
        // snake_case secrets), non-HITL tool, and orphan ID.
        let mut ev = event("ev-hitl-1", "test-agent");
        ev.long_running_tool_ids = vec![
            "call-cred".into(),
            "call-pause".into(),
            "call-orphan".into(),
        ];
        ev.response.content = Some(content(
            "model",
            vec![
                call_part(
                    "call-cred",
                    HITL_CREDENTIAL_FUNCTION_CALL_NAME,
                    json!({
                        "auth_scheme": "oauth2",
                        "access_token": "snake-secret",
                        "auth_code": "snake-auth-code-secret",
                        "code": "raw-oauth-code-secret",
                        "code_verifier": "snake-verifier-secret",
                        "api_key_string": "snake-api-key-string-secret",
                        "api_key_secret": "snake-api-key-secret-val",
                        "redirect_uri": "https://app.example.com/cb?code=uri-callback-code-secret&state=pub",
                        "authConfig": {"exchangedAuthCredential": {"oauth2": {
                            "accessToken": "camel-access-secret",
                            "refreshToken": "camel-refresh-secret",
                            "clientSecret": "camel-client-secret",
                            "authCode": "camel-auth-code-secret",
                            "codeVerifier": "camel-verifier-secret",
                            "apiKeyString": "camel-api-key-string-secret",
                            "apiKeySecret": "camel-api-key-secret-val",
                            "clientId": "public-client-id",
                        }}},
                    }),
                ),
                call_part(
                    "call-pause",
                    "deploy_cluster",
                    json!({"region": "us-central1", "code": "E_OK"}),
                ),
            ],
        ));
        on_event(&tp, &tp.ctx, ev).await;

        // 2a. Runtime confirmation flow step 1: tool-response event carrying
        // Actions.RequestedToolConfirmations (plus RequestedInput).
        // RequestedToolConfirmations alone must NOT emit a duplicate
        // HITL_CONFIRMATION_REQUEST.
        let mut ev = event("ev-hitl-2a", "test-agent");
        ev.actions.requested_tool_confirmations.insert(
            "orig-call-1".into(),
            ToolConfirmation {
                hint: "Approve deletion?".into(),
                ..ToolConfirmation::default()
            },
        );
        ev.requested_input = Some(RequestInput {
            interrupt_id: "int-1".into(),
            message: "Which environment?".into(),
            ..RequestInput::default()
        });
        on_event(&tp, &tp.ctx, ev).await;

        // 2b. Runtime confirmation flow step 2: synthesized adk_request_confirmation
        // FunctionCall event with its call ID in LongRunningToolIDs. Emits one
        // HITL_CONFIRMATION_REQUEST and one TOOL_PAUSED.
        let mut ev = event("ev-hitl-2b", "test-agent");
        ev.long_running_tool_ids = vec!["call-confirm-1".into()];
        ev.response.content = Some(content(
            "model",
            vec![call_part(
                "call-confirm-1",
                REQUEST_CONFIRMATION_FUNCTION_CALL_NAME,
                json!({"hint": "Approve deletion?"}),
            )],
        ));
        on_event(&tp, &tp.ctx, ev).await;

        // 3. Session event with HITL FunctionResponse completion containing camelCase &
        // snake_case secrets.
        let mut ev = event("ev-hitl-3", "test-agent");
        ev.response.content = Some(content(
            "user",
            vec![response_part(
                "call-cred",
                HITL_CREDENTIAL_FUNCTION_CALL_NAME,
                json!({
                    "status": "granted",
                    "accessToken": "granted-camel-secret",
                    "refreshToken": "granted-refresh-secret",
                    "api_key": "granted-snake-secret",
                    "auth_code": "granted-auth-code-secret",
                    "code": "granted-raw-code-secret",
                    "codeVerifier": "granted-verifier-secret",
                    "apiKeyString": "granted-api-key-string-secret",
                    "apiKeySecret": "granted-api-key-secret-val",
                }),
            )],
        ));
        on_event(&tp, &tp.ctx, ev).await;

        // 4. User message carrying HITL confirmation response and resumed tool response.
        on_user_message(
            &tp,
            &tp.ctx,
            content(
                "user",
                vec![
                    response_part(
                        "call-confirm-1",
                        REQUEST_CONFIRMATION_FUNCTION_CALL_NAME,
                        json!({"confirmed": true}),
                    ),
                    response_part(
                        "call-pause",
                        "deploy_cluster",
                        json!({"cluster_id": "c-123", "code": "E_OK"}),
                    ),
                ],
            ),
        )
        .await;

        let rows = finish(&tp).await;
        assert_eq!(
            event_types(&rows),
            [
                EVENT_HITL_CREDENTIAL_REQUEST,
                EVENT_TOOL_PAUSED, // hitl_credential
                EVENT_TOOL_PAUSED, // deploy_cluster ("tool")
                EVENT_TOOL_PAUSED, // call-orphan ("tool")
                EVENT_HITL_INPUT_REQUEST,
                EVENT_HITL_CONFIRMATION_REQUEST,
                EVENT_TOOL_PAUSED, // hitl_confirmation
                EVENT_HITL_CREDENTIAL_COMPLETED,
                EVENT_USER_MESSAGE_RECEIVED,
                EVENT_HITL_CONFIRMATION_COMPLETED,
                EVENT_TOOL_COMPLETED,
                EVENT_INVOCATION_COMPLETED,
            ],
            "event types"
        );

        let want_scrubbed_args = r#"{"tool_id":"call-cred","tool":"adk_request_credential","args":{"auth_scheme":"oauth2","access_token":"[REDACTED]","auth_code":"[REDACTED]","code":"raw-oauth-code-secret","code_verifier":"[REDACTED]","api_key_string":"[REDACTED]","api_key_secret":"[REDACTED]","redirect_uri":"https://app.example.com/cb?code=uri-callback-code-secret&state=pub","authConfig":{"exchangedAuthCredential":{"oauth2":{"accessToken":"[REDACTED]","refreshToken":"[REDACTED]","clientSecret":"[REDACTED]","authCode":"[REDACTED]","codeVerifier":"[REDACTED]","apiKeyString":"[REDACTED]","apiKeySecret":"[REDACTED]","clientId":"public-client-id"}}}}}"#;

        let cred_req = only_row_of_type(&rows, EVENT_HITL_CREDENTIAL_REQUEST);
        check_json(
            "HITL_CREDENTIAL_REQUEST content",
            &json_object_column(&cred_req, "content"),
            want_scrubbed_args,
        );
        check_json_at(
            &json_object_column(&cred_req, "attributes"),
            r#""call-cred""#,
            &["adk", "function_call_id"],
        );

        let confirm_req = only_row_of_type(&rows, EVENT_HITL_CONFIRMATION_REQUEST);
        check_json(
            "HITL_CONFIRMATION_REQUEST content",
            &json_object_column(&confirm_req, "content"),
            r#"{"tool_id":"call-confirm-1","tool":"adk_request_confirmation","args":{"hint":"Approve deletion?"}}"#,
        );

        let paused_rows = rows_of_type(&rows, EVENT_TOOL_PAUSED);
        assert_eq!(paused_rows.len(), 4, "TOOL_PAUSED rows");
        let attrs = |row: &Row| json_object_column(row, "attributes");
        check_json(
            "TOOL_PAUSED hitl_credential content",
            &json_object_column(&paused_rows[0], "content"),
            want_scrubbed_args,
        );
        check_json_at(
            &attrs(&paused_rows[0]),
            r#""hitl_credential""#,
            &["adk", "pause_kind"],
        );
        check_json_at(
            &attrs(&paused_rows[0]),
            r#""call-cred""#,
            &["adk", "function_call_id"],
        );

        check_json(
            "TOOL_PAUSED matched tool content",
            &json_object_column(&paused_rows[1], "content"),
            r#"{"tool_id":"call-pause","tool":"deploy_cluster","args":{"region":"us-central1","code":"E_OK"}}"#,
        );
        check_json_at(&attrs(&paused_rows[1]), r#""tool""#, &["adk", "pause_kind"]);
        check_json_at(
            &attrs(&paused_rows[1]),
            r#""call-pause""#,
            &["adk", "function_call_id"],
        );

        check_json(
            "TOOL_PAUSED orphan content",
            &json_object_column(&paused_rows[2], "content"),
            r#"{"tool_id":"call-orphan","tool":null,"args":null}"#,
        );
        check_json_at(&attrs(&paused_rows[2]), r#""tool""#, &["adk", "pause_kind"]);

        check_json(
            "TOOL_PAUSED hitl_confirmation content",
            &json_object_column(&paused_rows[3], "content"),
            r#"{"tool_id":"call-confirm-1","tool":"adk_request_confirmation","args":{"hint":"Approve deletion?"}}"#,
        );
        check_json_at(
            &attrs(&paused_rows[3]),
            r#""hitl_confirmation""#,
            &["adk", "pause_kind"],
        );
        check_json_at(
            &attrs(&paused_rows[3]),
            r#""call-confirm-1""#,
            &["adk", "function_call_id"],
        );

        let cred_completed = only_row_of_type(&rows, EVENT_HITL_CREDENTIAL_COMPLETED);
        check_json(
            "HITL_CREDENTIAL_REQUEST_COMPLETED content",
            &json_object_column(&cred_completed, "content"),
            r#"{"tool_id":"call-cred","tool":"adk_request_credential","result":{"status":"granted","accessToken":"[REDACTED]","refreshToken":"[REDACTED]","api_key":"[REDACTED]","auth_code":"[REDACTED]","code":"granted-raw-code-secret","codeVerifier":"[REDACTED]","apiKeyString":"[REDACTED]","apiKeySecret":"[REDACTED]"}}"#,
        );

        let confirm_completed = only_row_of_type(&rows, EVENT_HITL_CONFIRMATION_COMPLETED);
        check_json(
            "HITL_CONFIRMATION_COMPLETED content",
            &json_object_column(&confirm_completed, "content"),
            r#"{"tool_id":"call-confirm-1","tool":"adk_request_confirmation","result":{"confirmed":true}}"#,
        );

        let resumed_tool = only_row_of_type(&rows, EVENT_TOOL_COMPLETED);
        check_json(
            "resumed TOOL_COMPLETED content",
            &json_object_column(&resumed_tool, "content"),
            r#"{"tool":"deploy_cluster","result":{"cluster_id":"c-123","code":"E_OK"},"tool_origin":"UNKNOWN"}"#,
        );
        check_json_at(&attrs(&resumed_tool), r#""tool""#, &["adk", "pause_kind"]);
        check_json_at(
            &attrs(&resumed_tool),
            r#""call-pause""#,
            &["adk", "function_call_id"],
        );
    }

    // HITL credential scrubbing redacts typed maps, struct slices, and cyclic payloads while
    // propagating is_truncated.
    //
    // Adapted: gadk function-call args and responses are JSON, which has no typed maps,
    // struct slices or cycles: Go's typed values are their JSON encodings, and each cyclic map
    // is the same map nested once in itself (so no row holds Go's `[CIRCULAR_REFERENCE]`
    // placeholder; its secrets are still redacted at both depths).
    {
        let tp = plugin_with(|c| c.max_content_len = 20).await;
        let note = "a".repeat(50);
        let detail = "b".repeat(50);
        let cyclic_args = {
            let mut m = obj(json!({"accessToken": "cycle-args-secret", "note": note}));
            m.insert("oauth2".into(), Value::Object(m.clone()));
            Value::Object(m)
        };
        let cyclic_resp = {
            let mut m = obj(json!({"refreshToken": "cycle-resp-secret", "detail": detail}));
            m.insert("self".into(), Value::Object(m.clone()));
            Value::Object(m)
        };

        // 1. Long-running credential request with typed map, typed struct slice, and cyclic
        // map.
        let mut ev = event("ev-cred-typed-1", "test-agent");
        ev.long_running_tool_ids = vec!["call-cred-typed".into()];
        ev.response.content = Some(content(
            "model",
            vec![call_part(
                "call-cred-typed",
                HITL_CREDENTIAL_FUNCTION_CALL_NAME,
                json!({
                    "typedMap": {"accessToken": "typed-map-secret", "clientId": "public-id"},
                    "tokens": [{
                        "accessToken": "struct-access-secret",
                        "refreshToken": "struct-refresh-secret",
                        "clientId": "pub",
                    }],
                    "cyclic": cyclic_args,
                }),
            )],
        ));
        on_event(&tp, &tp.ctx, ev).await;

        // 2. Non-long-running credential request and session-event completion with
        // cyclic/typed payloads.
        let mut ev = event("ev-cred-typed-2", "test-agent");
        ev.response.content = Some(content(
            "model",
            vec![
                call_part(
                    "call-cred-unmatched",
                    HITL_CREDENTIAL_FUNCTION_CALL_NAME,
                    json!({"cyclic": cyclic_args}),
                ),
                response_part(
                    "call-cred-unmatched",
                    HITL_CREDENTIAL_FUNCTION_CALL_NAME,
                    json!({
                        "tokens": [{
                            "accessToken": "resp-struct-secret",
                            "refreshToken": "",
                            "clientId": "pub-resp",
                        }],
                        "cyclic": cyclic_resp,
                    }),
                ),
            ],
        ));
        on_event(&tp, &tp.ctx, ev).await;

        // 3. User-message credential completion with camelCase accessToken, cyclic, and typed
        // payloads.
        on_user_message(
            &tp,
            &tp.ctx,
            content(
                "user",
                vec![response_part(
                    "call-cred-typed",
                    HITL_CREDENTIAL_FUNCTION_CALL_NAME,
                    json!({
                        "accessToken": "user-msg-access-token-secret",
                        "typedMap": {"apiKey": "user-msg-api-key-secret"},
                        "cyclic": cyclic_resp,
                    }),
                )],
            ),
        )
        .await;

        let rows = finish(&tp).await;
        assert_eq!(
            event_types(&rows),
            [
                EVENT_HITL_CREDENTIAL_REQUEST,
                EVENT_TOOL_PAUSED,
                EVENT_HITL_CREDENTIAL_REQUEST,
                EVENT_HITL_CREDENTIAL_COMPLETED,
                EVENT_USER_MESSAGE_RECEIVED,
                EVENT_HITL_CREDENTIAL_COMPLETED,
                EVENT_INVOCATION_COMPLETED,
            ],
            "event types"
        );
        let secrets = [
            "typed-map-secret",
            "struct-access-secret",
            "struct-refresh-secret",
            "cycle-args-secret",
            "cycle-resp-secret",
            "resp-struct-secret",
            "user-msg-access-token-secret",
            "user-msg-api-key-secret",
        ];
        for r in &rows {
            let ev_type = str_col(r, "event_type");
            if ev_type == EVENT_INVOCATION_COMPLETED {
                continue;
            }
            let raw_row = row_text(r);
            for secret in secrets {
                assert!(
                    !raw_row.contains(secret),
                    "{ev_type} leaked secret {secret:?} in row: {raw_row}"
                );
            }
            assert_eq!(is_truncated(r), Some(true), "{ev_type} is_truncated");
        }

        let req_rows = rows_of_type(&rows, EVENT_HITL_CREDENTIAL_REQUEST);
        let req_content = json_object_column(&req_rows[0], "content");
        check_json_at(
            &req_content,
            r#"{"accessToken":"[REDACTED]","clientId":"public-id"}"#,
            &["args", "typedMap"],
        );
        check_json_at(
            &req_content,
            r#"[{"accessToken":"[REDACTED]","refreshToken":"[REDACTED]","clientId":"pub"}]"#,
            &["args", "tokens"],
        );
        check_json_at(
            &req_content,
            r#""[REDACTED]""#,
            &["args", "cyclic", "oauth2", "accessToken"],
        );

        let completed_rows = rows_of_type(&rows, EVENT_HITL_CREDENTIAL_COMPLETED);
        let user_msg_completed_content = json_object_column(&completed_rows[1], "content");
        check_json_at(
            &user_msg_completed_content,
            r#""[REDACTED]""#,
            &["result", "accessToken"],
        );
        check_json_at(
            &user_msg_completed_content,
            r#"{"apiKey":"[REDACTED]"}"#,
            &["result", "typedMap"],
        );
    }

    // HITL adk_request_input LongRunningToolIDs, single ID-less call fallback, and
    // non-long-running HITL calls
    {
        let tp = setup_test_plugin().await;
        // 1. adk_request_input with empty FunctionCall.ID matched via single-call fallback in
        // LongRunningToolIDs, plus ev.RequestedInput on the same event (must NOT double-emit
        // HITL_INPUT_REQUEST).
        let mut ev = event("ev-input-lr", "workflow-agent");
        ev.long_running_tool_ids = vec!["resolved-input-1".into()];
        ev.requested_input = Some(RequestInput {
            interrupt_id: "int-dup".into(),
            message: "Enter approval code".into(),
            ..RequestInput::default()
        });
        ev.response.content = Some(content(
            "model",
            vec![call_part(
                "",
                WORKFLOW_INPUT_FUNCTION_CALL_NAME,
                json!({"prompt": "Enter approval code"}),
            )],
        ));
        on_event(&tp, &tp.ctx, ev).await;

        // 2. HITL FunctionCall without LongRunningToolIDs emits HITL_CONFIRMATION_REQUEST
        // without TOOL_PAUSED.
        let mut ev = event("ev-hitl-no-lr", "workflow-agent");
        ev.response.content = Some(content(
            "model",
            vec![call_part(
                "call-confirm-no-lr",
                REQUEST_CONFIRMATION_FUNCTION_CALL_NAME,
                json!({"hint": "Continue?"}),
            )],
        ));
        on_event(&tp, &tp.ctx, ev).await;

        let rows = finish(&tp).await;
        assert_eq!(
            event_types(&rows),
            [
                EVENT_HITL_INPUT_REQUEST,
                EVENT_TOOL_PAUSED,
                EVENT_HITL_CONFIRMATION_REQUEST,
                EVENT_INVOCATION_COMPLETED,
            ],
            "event types"
        );
        let input_req = only_row_of_type(&rows, EVENT_HITL_INPUT_REQUEST);
        check_json(
            "HITL_INPUT_REQUEST content",
            &json_object_column(&input_req, "content"),
            r#"{"tool_id":"resolved-input-1","tool":"adk_request_input","args":{"prompt":"Enter approval code"}}"#,
        );
        let paused = only_row_of_type(&rows, EVENT_TOOL_PAUSED);
        let attrs = json_object_column(&paused, "attributes");
        check_json_at(&attrs, r#""hitl_input""#, &["adk", "pause_kind"]);
        check_json_at(
            &attrs,
            r#""resolved-input-1""#,
            &["adk", "function_call_id"],
        );
    }

    // A2A_INTERACTION, NODE_OUTPUT, NODE_ERROR, and multi-signal orthogonality
    {
        let tp = setup_test_plugin().await;
        // 1. Correlation-only A2A metadata (without a2a:request or a2a:response) must NOT emit
        // A2A_INTERACTION, and MessageAsOutput=true must NOT emit NODE_OUTPUT.
        let mut ev = event("ev-corr-only", "remote-worker");
        ev.node_info = Some(NodeInfo {
            path: "wf@0/msg@0".into(),
            message_as_output: true,
            ..NodeInfo::default()
        });
        ev.output = Some(serde_json::to_value(text_content("model", "message output")).unwrap());
        ev.response.content = Some(text_content("model", "message output"));
        ev.response.custom_metadata = obj(json!({
            "a2a:task_id": "task-corr-only",
            "a2a:context_id": "ctx-corr-only",
        }));
        on_event(&tp, &tp.ctx, ev).await;

        // 1b. NodeInfo with empty Path must NOT emit NODE_ERROR or NODE_OUTPUT.
        let mut ev = event("ev-empty-node-path", "remote-worker");
        ev.node_info = Some(NodeInfo::default());
        ev.output = Some(json!({"ignored": true}));
        ev.response.error_code = "DEADLINE_EXCEEDED".into();
        ev.response.error_message = "node with empty path".into();
        on_event(&tp, &tp.ctx, ev).await;

        // 2. Single event with multiple orthogonal signals: StateDelta, TransferToAgent, A2A
        // interaction payload (with non-nil ev.Content, verifying A2A content uses protocol
        // response), NodeInfo output + node failure on the same event, and final
        // AGENT_RESPONSE text.
        let mut ev = event("ev-multi-1", "remote-worker");
        ev.branch = "root.remote-worker".into();
        ev.node_info = Some(NodeInfo {
            path: "wf@0/step@1".into(),
            ..NodeInfo::default()
        });
        ev.output = Some(json!({"node_result": "partial_done"}));
        ev.actions.state_delta = Some(obj(json!({"progress": "100%"})));
        ev.actions.transfer_to_agent = "reviewer-agent".into();
        ev.response.content = Some(text_content("model", "all tasks completed"));
        ev.response.error_code = "DEADLINE_EXCEEDED".into();
        ev.response.error_message =
            "node failed with Authorization: Bearer secret-node-token".into();
        ev.response.custom_metadata = obj(json!({
            "a2a:task_id": "task-99",
            "a2a:context_id": "ctx-42",
            "a2a:response": {"protocol_status": "completed"},
            "a2a:custom": "colon-val",
            "a2a_underscore": "ignored",
            "non_a2a_key": "ignored",
        }));
        on_event(&tp, &tp.ctx, ev).await;

        // 3. LLM termination/blocking code (e.g. SAFETY) on a workflow node must NOT emit
        // NODE_ERROR, and AGENT_RESPONSE must remain status="OK".
        let mut ev = event("ev-node-safety", "remote-worker");
        ev.node_info = Some(NodeInfo {
            path: "wf@0/step@2".into(),
            ..NodeInfo::default()
        });
        ev.response.content = Some(text_content("model", "safety blocked response"));
        ev.response.error_code = "SAFETY".into();
        ev.response.error_message = "Blocked by safety filter".into();
        on_event(&tp, &tp.ctx, ev).await;

        let rows = finish(&tp).await;
        assert_eq!(
            event_types(&rows),
            [
                EVENT_AGENT_RESPONSE, // from ev-corr-only (no A2A_INTERACTION, no NODE_OUTPUT)
                EVENT_STATE_DELTA,
                EVENT_AGENT_TRANSFER,
                EVENT_A2A_INTERACTION,
                EVENT_NODE_ERROR,
                EVENT_NODE_OUTPUT,
                EVENT_AGENT_RESPONSE, // from ev-multi-1
                EVENT_AGENT_RESPONSE, // from ev-node-safety (no NODE_ERROR)
                EVENT_INVOCATION_COMPLETED,
            ],
            "event types"
        );

        let a2a_row = only_row_of_type(&rows, EVENT_A2A_INTERACTION);
        check_json_at(
            &json_object_column(&a2a_row, "attributes"),
            r#"{"a2a:task_id":"task-99","a2a:context_id":"ctx-42","a2a:response":{"protocol_status":"completed"},"a2a:custom":"colon-val"}"#,
            &["a2a_metadata"],
        );
        check_json(
            "A2A protocol response content",
            &json_object_column(&a2a_row, "content"),
            r#"{"protocol_status":"completed"}"#,
        );

        let node_out = only_row_of_type(&rows, EVENT_NODE_OUTPUT);
        check_json(
            "NODE_OUTPUT content",
            &json_object_column(&node_out, "content"),
            r#"{"node_result":"partial_done"}"#,
        );

        let node_err = only_row_of_type(&rows, EVENT_NODE_ERROR);
        assert_eq!(str_col(&node_err, "status"), "ERROR", "NODE_ERROR status");
        assert_eq!(
            str_col(&node_err, "error_message"),
            "node failed with Authorization: [REDACTED]",
            "NODE_ERROR error_message"
        );
        // NODE_ERROR content must NOT duplicate raw error_message or leak Bearer tokens.
        check_json(
            "NODE_ERROR content",
            &json_object_column(&node_err, "content"),
            r#"{"node_path":"wf@0/step@1","error_code":"DEADLINE_EXCEEDED"}"#,
        );
        let content_str = str_col(&node_err, "content");
        assert!(
            !content_str.contains("secret-node-token"),
            "NODE_ERROR content leaked bearer token: {content_str}"
        );

        // Verify all AGENT_RESPONSE rows have status="OK", even when ev.ErrorCode was set.
        for resp_row in rows_of_type(&rows, EVENT_AGENT_RESPONSE) {
            assert_eq!(str_col(&resp_row, "status"), "OK", "AGENT_RESPONSE status");
        }
    }

    // streaming partial events do not emit A2A_INTERACTION, NODE_OUTPUT, or NODE_ERROR
    {
        let tp = setup_test_plugin().await;
        // Partial event carrying NodeInfo, Output, ErrorCode/ErrorMessage, and a2a:* metadata
        // must not emit A2A_INTERACTION, NODE_OUTPUT, or NODE_ERROR.
        let mut ev = event("ev-partial-signals", "remote-worker");
        ev.node_info = Some(NodeInfo {
            path: "wf@0/step@1".into(),
            ..NodeInfo::default()
        });
        ev.output = Some(json!({"chunk": "data"}));
        ev.response.partial = true;
        ev.response.error_code = "DEADLINE_EXCEEDED".into();
        ev.response.error_message = "stream interrupted".into();
        ev.response.custom_metadata = obj(json!({
            "a2a:task_id": "task-stream-1",
            "a2a:response": {"chunk": 1},
        }));
        on_event(&tp, &tp.ctx, ev).await;
        let rows = finish(&tp).await;
        assert_eq!(
            event_types(&rows),
            [EVENT_INVOCATION_COMPLETED],
            "event types"
        );
    }
}

#[tokio::test]
async fn test_session_events_legacy_event_filtering() {
    let all_non_session_types = [
        EVENT_USER_MESSAGE_RECEIVED,
        EVENT_TOOL_STARTING,
        EVENT_TOOL_COMPLETED,
        EVENT_INVOCATION_COMPLETED,
    ];
    let with_non_session = |head: &[&str]| -> Vec<String> {
        head.iter()
            .chain(all_non_session_types.iter())
            .map(|s| (*s).to_owned())
            .collect()
    };
    // (name, denylist, allowlist, want types)
    type FilterCase = (&'static str, Vec<String>, Vec<String>, Vec<&'static str>);
    let cases: [FilterCase; 3] = [
        (
            "legacy EVENT in EventDenylist suppresses session-event rows across all emitters",
            vec!["EVENT".into()],
            Vec::new(),
            vec![
                EVENT_USER_MESSAGE_RECEIVED,
                EVENT_TOOL_STARTING,
                EVENT_TOOL_COMPLETED,
                EVENT_AGENT_RESPONSE,
                EVENT_INVOCATION_COMPLETED,
            ],
        ),
        (
            "legacy EVENT in EventAllowlist permits session-event rows across all emitters",
            Vec::new(),
            with_non_session(&["EVENT", EVENT_AGENT_RESPONSE]),
            vec![
                EVENT_USER_MESSAGE_RECEIVED,
                EVENT_HITL_CREDENTIAL_COMPLETED,
                EVENT_TOOL_COMPLETED,
                EVENT_STATE_DELTA,
                EVENT_AGENT_RESPONSE,
                EVENT_HITL_CONFIRMATION_COMPLETED,
                EVENT_TOOL_STARTING,
                EVENT_TOOL_COMPLETED,
                EVENT_AGENT_RESPONSE,
                EVENT_INVOCATION_COMPLETED,
            ],
        ),
        (
            "per-type denylist takes precedence over legacy EVENT allowlist",
            vec![
                EVENT_AGENT_RESPONSE.into(),
                EVENT_HITL_CREDENTIAL_COMPLETED.into(),
            ],
            with_non_session(&["EVENT"]),
            vec![
                EVENT_USER_MESSAGE_RECEIVED,
                EVENT_TOOL_COMPLETED,
                EVENT_STATE_DELTA,
                EVENT_HITL_CONFIRMATION_COMPLETED,
                EVENT_TOOL_STARTING,
                EVENT_TOOL_COMPLETED,
                EVENT_INVOCATION_COMPLETED,
            ],
        ),
    ];
    for (name, denylist, allowlist, want_types) in cases {
        let tp = plugin_with(|c| {
            c.event_denylist = denylist;
            c.event_allowlist = allowlist;
            c.final_response_tool_names = vec!["final_answer".into()];
        })
        .await;
        // 1. User message carrying both a HITL credential completion and a resumed tool
        // completion.
        on_user_message(
            &tp,
            &tp.ctx,
            content(
                "user",
                vec![
                    response_part(
                        "call-cred-1",
                        HITL_CREDENTIAL_FUNCTION_CALL_NAME,
                        json!({"ok": true}),
                    ),
                    response_part("call-resumed-1", "fetch_data", json!({"rows": 3})),
                ],
            ),
        )
        .await;
        // 2a. Final session event carrying StateDelta and visible text.
        let mut ev = event("ev-filter-1", "test-agent");
        ev.actions.state_delta = Some(obj(json!({"k": "v"})));
        ev.response.content = Some(text_content("model", "hello"));
        on_event(&tp, &tp.ctx, ev).await;
        // 2b. Session event carrying HITL confirmation completion.
        let mut ev = event("ev-filter-2", "test-agent");
        ev.response.content = Some(content(
            "user",
            vec![response_part(
                "call-confirm-1",
                REQUEST_CONFIRMATION_FUNCTION_CALL_NAME,
                json!({"confirmed": true}),
            )],
        ));
        on_event(&tp, &tp.ctx, ev).await;
        // 3. FinalResponseToolNames tool call emitting TOOL_STARTING, TOOL_COMPLETED, and
        // AGENT_RESPONSE.
        let final_ctx = tp.ctx.with_function_call("call-final-1").context();
        let final_tool = FakeTool::named("final_answer");
        let args = obj(json!({"q": "hi"}));
        before_tool(&tp, &final_ctx, &final_tool, &args).await;
        after_tool(
            &tp,
            &final_ctx,
            &final_tool,
            &args,
            obj(json!({"answer": "done"})),
            None,
        )
        .await;
        let rows = finish(&tp).await;
        assert_eq!(event_types(&rows), want_types, "{name}: event types");
    }
}

// parity: adapted: Go's nil `*genai.Part` is an empty `Part::default()`
#[tokio::test]
async fn test_agent_response_content_formatter_and_nil_parts() {
    // ContentFormatter scrubbing does not leak raw text into content_parts
    {
        let tp = plugin_with(|c| {
            c.content_formatter = Some(Arc::new(|payload: &Value, event_type: &str| {
                if event_type == EVENT_AGENT_RESPONSE {
                    return Ok(json!({"response": "[REDACTED]"}));
                }
                Ok(payload.clone())
            }));
        })
        .await;
        let mut ev = event("ev-fmt-1", "test-agent");
        ev.response.content = Some(text_content("model", "top-secret-value"));
        on_event(&tp, &tp.ctx, ev).await;
        let rows = finish(&tp).await;
        let row = only_row_of_type(&rows, EVENT_AGENT_RESPONSE);
        check_json(
            "formatted AGENT_RESPONSE content",
            &json_object_column(&row, "content"),
            r#"{"response":"[REDACTED]"}"#,
        );
        if let Some(RowValue::Parts(parts)) = row.get("content_parts") {
            for part in parts {
                if let Some(RowValue::Str(text)) = part.get("text") {
                    assert!(
                        !text.contains("top-secret-value"),
                        "content_parts leaked unformatted text: {parts:?}"
                    );
                }
            }
        }
    }

    // nil entries at beginning and end of Parts slice do not panic
    //
    // Adapted: a gadk `Content` holds `Part` values, not nillable pointers; Go's nil part is
    // an empty `Part::default()`, which holds nothing to log either.
    {
        let tp = setup_test_plugin().await;
        let mut ev = event("ev-nil-start", "test-agent");
        ev.response.content = Some(content(
            "model",
            vec![Part::default(), text_part_of("leading nil", false)],
        ));
        on_event(&tp, &tp.ctx, ev).await;
        let mut ev = event("ev-nil-end", "test-agent");
        ev.response.content = Some(content(
            "model",
            vec![text_part_of("trailing nil", false), Part::default()],
        ));
        on_event(&tp, &tp.ctx, ev).await;
        let rows = finish(&tp).await;
        let resp_rows = rows_of_type(&rows, EVENT_AGENT_RESPONSE);
        assert_eq!(resp_rows.len(), 2, "AGENT_RESPONSE rows");
        check_json(
            "leading nil content",
            &json_object_column(&resp_rows[0], "content"),
            r#"{"response":"text: 'leading nil'"}"#,
        );
        check_json(
            "trailing nil content",
            &json_object_column(&resp_rows[1], "content"),
            r#"{"response":"text: 'trailing nil'"}"#,
        );
    }
}

#[tokio::test]
async fn test_agent_response_skips_thought_only_and_partial_events() {
    let tp = setup_test_plugin().await;

    // 1. Partial event -> skipped.
    let mut ev = event("ev-partial", "test-agent");
    ev.response.partial = true;
    ev.response.content = Some(text_content("model", "streaming chunk"));
    on_event(&tp, &tp.ctx, ev).await;

    // 2. User-authored event -> skipped.
    let mut ev = event("ev-user", "user");
    ev.response.content = Some(text_content("user", "user text"));
    on_event(&tp, &tp.ctx, ev).await;

    // 3. Thought-only event -> skipped.
    let mut ev = event("ev-thought-only", "test-agent");
    ev.response.content = Some(content(
        "model",
        vec![text_part_of("internal chain of thought", true)],
    ));
    on_event(&tp, &tp.ctx, ev).await;

    // 4. Event with FunctionCall -> skipped.
    let mut ev = event("ev-tool-call", "test-agent");
    ev.response.content = Some(content(
        "model",
        vec![
            text_part_of("calling tool", false),
            call_part("", "lookup", json!({})),
        ],
    ));
    on_event(&tp, &tp.ctx, ev).await;

    // 5. Response-only SkipSummarization acknowledgement -> skipped (no visible text).
    let mut ev = event("ev-skip-summary", "test-agent");
    ev.actions.skip_summarization = true;
    ev.response.content = Some(content(
        "model",
        vec![response_part("call-ack-1", "lookup", json!({"ok": true}))],
    ));
    on_event(&tp, &tp.ctx, ev).await;

    // 5b. SkipSummarization event with visible text (such as agenttool display) -> emits
    // AGENT_RESPONSE.
    let mut ev = event("ev-skip-summary-visible", "test-agent");
    ev.actions.skip_summarization = true;
    ev.response.content = Some(content(
        "model",
        vec![
            response_part(
                "call-agent-1",
                "sub_agent",
                json!({"result": "sub-agent answer"}),
            ),
            text_part_of("sub-agent answer", false),
        ],
    ));
    on_event(&tp, &tp.ctx, ev).await;

    // 6. Final response mixing Thought and visible Text -> emits only visible Text.
    let mut ev = event("ev-final-mixed", "test-agent");
    ev.branch = "root.test-agent".into();
    ev.response.content = Some(content(
        "model",
        vec![
            text_part_of("hidden reasoning", true),
            text_part_of("visible final answer", false),
        ],
    ));
    on_event(&tp, &tp.ctx, ev).await;

    let rows = finish(&tp).await;
    assert_eq!(
        event_types(&rows),
        [
            EVENT_AGENT_RESPONSE,
            EVENT_AGENT_RESPONSE,
            EVENT_INVOCATION_COMPLETED
        ],
        "event types"
    );
    let resp_rows = rows_of_type(&rows, EVENT_AGENT_RESPONSE);
    check_json(
        "SkipSummarization AGENT_RESPONSE content",
        &json_object_column(&resp_rows[0], "content"),
        r#"{"response":"text: 'sub-agent answer'"}"#,
    );
    let row = &resp_rows[1];
    check_json(
        "AGENT_RESPONSE content",
        &json_object_column(row, "content"),
        r#"{"response":"text: 'visible final answer'"}"#,
    );
    let got_parts = match row.get("content_parts") {
        Some(RowValue::Parts(parts)) => parts.clone(),
        _ => Vec::new(),
    };
    assert_eq!(
        got_parts,
        vec![text_part("visible final answer", 0)],
        "content_parts"
    );
}

#[test]
fn test_adk_envelope_extras_cannot_overwrite_core_keys() {
    let ap = AnalyticsPlugin {
        config: Config::default().apply_defaults(),
        traces: Some(Arc::new(TraceManager::new())),
        ..AnalyticsPlugin::default()
    };
    let id = Identity {
        agent_name: "agent-name".into(),
        session_id: "sess-id".into(),
        invocation_id: "inv-id".into(),
        user_id: "user-id".into(),
        app_name: "test-app".into(),
        branch: "root.agent-name".into(),
        ..Identity::default()
    };
    let d = EventData {
        author: "event-author".into(),
        function_call_id: "call-core".into(),
        adk_extras: obj(json!({
            "schema_version": "999",
            "event_id": "spoofed-id",
            "app_name": "spoofed-app",
            "invocation_id": "spoofed-inv",
            "author": "spoofed-author",
            "function_call_id": "spoofed-call",
            "pause_kind": "hitl_confirmation",
        })),
        ..EventData::default()
    };
    let adk = ap.adk_envelope("real-event-id", &id, "agent-name", &d);
    for (key, want) in [
        ("schema_version", ADK_ENVELOPE_SCHEMA_VERSION),
        ("event_id", "real-event-id"),
        ("app_name", "test-app"),
        ("invocation_id", "inv-id"),
        ("author", "event-author"),
        ("function_call_id", "call-core"),
        ("pause_kind", "hitl_confirmation"),
    ] {
        assert_eq!(adk.get(key), Some(&json!(want)), "adk[{key}]");
    }
}

/// Go `swapTool`: a user wrapper whose inner tool (and agents) another thread swaps; it has
/// no `Unwrap`, so its inner tool is never read.
struct SwapTool {
    name: String,
    inner: parking_lot::RwLock<Swapped>,
}

/// What a [`SwapTool`] holds: Go's `innerTool` and `supportedAgents`.
type Swapped = (Arc<dyn Tool>, Vec<Arc<dyn gadk_agent::Agent>>);

impl Tool for SwapTool {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        "swap tool"
    }
    fn is_long_running(&self) -> bool {
        self.inner.read().0.is_long_running()
    }
    fn declaration(&self) -> Option<gadk_genai::FunctionDeclaration> {
        Some(gadk_genai::FunctionDeclaration {
            name: self.name.clone(),
            description: "swap tool".into(),
            ..gadk_genai::FunctionDeclaration::default()
        })
    }
    fn as_runnable(&self) -> Option<&dyn gadk_agent::RunnableTool> {
        Some(self)
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[async_trait::async_trait]
impl gadk_agent::RunnableTool for SwapTool {
    async fn run(
        &self,
        _ctx: gadk_agent::Context,
        _args: Value,
    ) -> Result<Map<String, Value>, ToolError> {
        Ok(obj(json!({"ok": true})))
    }
}

/// Go `swapUnwrapTool`: a user wrapper whose `Unwrap` reads the inner tool under its lock.
struct SwapUnwrapTool {
    name: String,
    inner: parking_lot::RwLock<Arc<dyn Tool>>,
}

impl Tool for SwapUnwrapTool {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        "swap unwrap tool"
    }
    fn is_long_running(&self) -> bool {
        self.inner.read().is_long_running()
    }
    fn declaration(&self) -> Option<gadk_genai::FunctionDeclaration> {
        Some(gadk_genai::FunctionDeclaration {
            name: self.name.clone(),
            description: "swap unwrap tool".into(),
            ..gadk_genai::FunctionDeclaration::default()
        })
    }
    fn as_runnable(&self) -> Option<&dyn gadk_agent::RunnableTool> {
        Some(self)
    }
    fn unwrap(&self) -> Option<Arc<dyn Tool>> {
        Some(Arc::clone(&self.inner.read()))
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[async_trait::async_trait]
impl gadk_agent::RunnableTool for SwapUnwrapTool {
    async fn run(
        &self,
        _ctx: gadk_agent::Context,
        _args: Value,
    ) -> Result<Map<String, Value>, ToolError> {
        Ok(obj(json!({"ok": true})))
    }
}

/// Runs `swap(i)` for i = 0, 1, … on a thread of its own until the returned guard is
/// dropped (Go's mutator goroutine, stopped by `close(done)` and `wg.Wait()`).
struct Mutator {
    done: Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Mutator {
    fn start(swap: impl Fn(usize) + Send + 'static) -> Mutator {
        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let handle = {
            let done = Arc::clone(&done);
            std::thread::spawn(move || {
                let mut i = 0;
                while !done.load(std::sync::atomic::Ordering::SeqCst) {
                    swap(i);
                    i += 1;
                }
            })
        };
        Mutator {
            done,
            handle: Some(handle),
        }
    }
}

impl Drop for Mutator {
    fn drop(&mut self) {
        self.done.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

// Go: no struct reflection, an `Unwrap` method under the wrapper's lock; Go's goroutine is a
// thread.
// parity: adapted: a user wrapper is read only through `Tool::unwrap`
#[tokio::test]
async fn test_unwrap_tool_value_concurrent_user_wrapper_no_race() {
    let fn_tool = function_tool(FunctionToolConfig::new("fn_swap", "Function tool"));
    let local_agent = local_agent("local_helper", "Local helper agent", Vec::new());
    let remote_a2a_agent = remote_agent("remote_specialist", "Remote A2A specialist");
    let remote_a2a_agent_tool = AgentTool::new(Arc::clone(&remote_a2a_agent), None);

    // user wrapper without Unwrap is not read via struct reflection
    {
        let tp = setup_test_plugin().await;
        let normal_swap = Arc::new(SwapTool {
            name: "user_swap_tool".into(),
            inner: parking_lot::RwLock::new((Arc::clone(&fn_tool), Vec::new())),
        });
        let transfer_swap = Arc::new(SwapTool {
            name: "transfer_to_agent".into(),
            inner: parking_lot::RwLock::new((
                Arc::clone(&fn_tool),
                vec![Arc::clone(&local_agent), Arc::clone(&remote_a2a_agent)],
            )),
        });
        let normal: Arc<dyn Tool> = normal_swap.clone();
        let transfer: Arc<dyn Tool> = transfer_swap.clone();
        let confirmed_swap = confirmed("swap_ts", Arc::clone(&normal)).await;
        let runner_agent = llm(LlmAgentConfig {
            name: "swap_runner".into(),
            tools: vec![Arc::clone(&normal)],
            ..LlmAgentConfig::default()
        });
        let run_ctx = MockCtx {
            agent: Some(runner_agent),
            ..tp.ctx.clone()
        };
        let mutator = {
            let (fn_tool, remote_tool) = (Arc::clone(&fn_tool), Arc::clone(&remote_a2a_agent_tool));
            let (local, remote) = (Arc::clone(&local_agent), Arc::clone(&remote_a2a_agent));
            let (normal_swap, transfer_swap) = (normal_swap.clone(), transfer_swap.clone());
            Mutator::start(move |i| {
                let (next_tool, next_agents) = if i % 2 == 1 {
                    (Arc::clone(&remote_tool), vec![Arc::clone(&remote)])
                } else {
                    (Arc::clone(&fn_tool), vec![Arc::clone(&local)])
                };
                normal_swap.inner.write().0 = Arc::clone(&next_tool);
                *transfer_swap.inner.write() = (next_tool, next_agents);
            })
        };
        let tool_ctx = tp.ctx.context();
        for i in 0..25 {
            let args = obj(json!({"iter": i}));
            let tx_args = obj(json!({"agent_name": "remote_specialist"}));
            before_tool(&tp, &tool_ctx, &normal, &args).await;
            after_tool(
                &tp,
                &tool_ctx,
                &normal,
                &args,
                obj(json!({"ok": true})),
                None,
            )
            .await;
            on_tool_error(
                &tp,
                &tool_ctx,
                &confirmed_swap,
                &args,
                Arc::new(ToolError::Execution("boom".into())),
            )
            .await
            .expect("OnToolErrorCallback");
            before_tool(&tp, &tool_ctx, &transfer, &tx_args).await;
            after_tool(
                &tp,
                &tool_ctx,
                &transfer,
                &tx_args,
                obj(json!({"ok": true})),
                None,
            )
            .await;
            on_user_message(
                &tp,
                &run_ctx,
                content(
                    "user",
                    vec![response_part(
                        &format!("call-swap-{i}"),
                        "user_swap_tool",
                        json!({"resumed": true}),
                    )],
                ),
            )
            .await;
        }
        drop(mutator);
        let rows = finish(&tp).await;
        assert!(
            !rows_of_type(&rows, EVENT_TOOL_STARTING).is_empty(),
            "expected TOOL_STARTING rows"
        );
    }

    // user wrapper with Unwrap acquires lock and resolves inner tool origin
    {
        let tp = setup_test_plugin().await;
        let unwrap_swap = Arc::new(SwapUnwrapTool {
            name: "user_unwrap_swap".into(),
            inner: parking_lot::RwLock::new(Arc::clone(&fn_tool)),
        });
        let unwrap_tool: Arc<dyn Tool> = unwrap_swap.clone();
        let confirmed_unwrap_swap = confirmed("unwrap_swap_ts", Arc::clone(&unwrap_tool)).await;
        let mutator = {
            let (fn_tool, remote_tool) = (Arc::clone(&fn_tool), Arc::clone(&remote_a2a_agent_tool));
            let unwrap_swap = unwrap_swap.clone();
            Mutator::start(move |i| {
                let next = if i % 2 == 1 {
                    Arc::clone(&remote_tool)
                } else {
                    Arc::clone(&fn_tool)
                };
                *unwrap_swap.inner.write() = next;
            })
        };
        let tool_ctx = tp.ctx.context();
        for i in 0..25 {
            let args = obj(json!({"iter": i}));
            before_tool(&tp, &tool_ctx, &unwrap_tool, &args).await;
            after_tool(
                &tp,
                &tool_ctx,
                &confirmed_unwrap_swap,
                &args,
                obj(json!({"ok": true})),
                None,
            )
            .await;
        }
        drop(mutator);
        let rows = finish(&tp).await;
        let mut tool_rows = rows_of_type(&rows, EVENT_TOOL_STARTING);
        tool_rows.extend(rows_of_type(&rows, EVENT_TOOL_COMPLETED));
        assert!(!tool_rows.is_empty(), "expected tool event rows");
        for row in &tool_rows {
            let content = json_object_column(row, "content");
            let origin = content["tool_origin"].as_str().unwrap_or_default();
            assert!(
                origin == TOOL_ORIGIN_LOCAL || origin == TOOL_ORIGIN_A2A,
                "tool_origin = {origin:?}, want {TOOL_ORIGIN_LOCAL:?} or {TOOL_ORIGIN_A2A:?}"
            );
        }
    }
}

// parity: adapted: Go's "no agent" case is MockCtx's default agent (a custom agent, no tools)
#[tokio::test]
async fn test_user_message_resumed_tool_records_tool_origin() {
    let deploy = function_tool(FunctionToolConfig {
        is_long_running: true,
        ..FunctionToolConfig::new("deploy_cluster", "Deploys a cluster")
    });
    let worker = llm(LlmAgentConfig {
        name: "worker".into(),
        tools: vec![Arc::clone(&deploy)],
        ..LlmAgentConfig::default()
    });
    let peer = llm(LlmAgentConfig {
        name: "peer".into(),
        ..LlmAgentConfig::default()
    });
    let root = llm(LlmAgentConfig {
        name: "root".into(),
        sub_agents: vec![Arc::clone(&worker), Arc::clone(&peer)],
        ..LlmAgentConfig::default()
    });
    // remoteDeploy has the name of deploy and another origin (fakeTool -> UNKNOWN).
    let remote_deploy: Arc<dyn Tool> = Arc::new(FakeTool {
        name: "deploy_cluster".into(),
        long_running: true,
        ..FakeTool::default()
    });
    let ambiguous_root = llm(LlmAgentConfig {
        name: "ambiguous_root".into(),
        tools: vec![remote_deploy],
        sub_agents: vec![llm(LlmAgentConfig {
            name: "local_worker".into(),
            tools: vec![Arc::clone(&deploy)],
            ..LlmAgentConfig::default()
        })],
        ..LlmAgentConfig::default()
    });

    type Case = (
        &'static str,
        Option<Arc<dyn gadk_agent::Agent>>,
        Option<Arc<dyn gadk_agent::Agent>>,
        &'static str,
        &'static str,
    );
    let cases: Vec<Case> = vec![
        (
            "tool of the agent that made the call",
            None,
            Some(Arc::clone(&worker)),
            "deploy_cluster",
            TOOL_ORIGIN_LOCAL,
        ),
        (
            "tool of a sub-agent",
            None,
            Some(Arc::clone(&root)),
            "deploy_cluster",
            TOOL_ORIGIN_LOCAL,
        ),
        (
            "tool elsewhere in a tree seen before",
            Some(Arc::clone(&root)),
            Some(Arc::clone(&peer)),
            "deploy_cluster",
            TOOL_ORIGIN_LOCAL,
        ),
        (
            "tool elsewhere in a tree not seen",
            None,
            Some(Arc::clone(&peer)),
            "deploy_cluster",
            TOOL_ORIGIN_UNKNOWN,
        ),
        (
            "unknown tool",
            None,
            Some(Arc::clone(&root)),
            "missing_tool",
            TOOL_ORIGIN_UNKNOWN,
        ),
        (
            "tools of different origins",
            None,
            Some(Arc::clone(&ambiguous_root)),
            "deploy_cluster",
            TOOL_ORIGIN_UNKNOWN,
        ),
        (
            "no agent",
            None,
            None,
            "deploy_cluster",
            TOOL_ORIGIN_UNKNOWN,
        ),
    ];
    for (name, seen, runner, tool, want) in cases {
        let tp = setup_test_plugin().await;
        if let Some(seen) = seen {
            let seen_ctx = MockCtx {
                agent: Some(seen),
                ..tp.ctx.clone()
            };
            before_run(&tp, &seen_ctx).await;
        }
        let ctx = MockCtx {
            agent: runner,
            ..tp.ctx.clone()
        };
        on_user_message(
            &tp,
            &ctx,
            content(
                "user",
                vec![response_part(
                    "call-pause",
                    tool,
                    json!({"cluster_id": "c-123"}),
                )],
            ),
        )
        .await;
        let rows = finish(&tp).await;
        let row = only_row_of_type(&rows, EVENT_TOOL_COMPLETED);
        assert_eq!(str_col(&row, "status"), "OK", "{name}: status");
        check_json_at(
            &json_object_column(&row, "content"),
            &format!("{want:?}"),
            &["tool_origin"],
        );
    }
}

/// Go `fmt.Errorf("error tool %q %w", name, err)`.
#[derive(Debug)]
struct NamedToolError {
    name: String,
    source: ToolError,
}

impl std::fmt::Display for NamedToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "error tool {} {}",
            gadk_agent::go_quote(&self.name),
            self.source
        )
    }
}

impl std::error::Error for NamedToolError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

#[tokio::test]
async fn test_tool_confirmation_rejected_completes_call() {
    let tp = plugin_with(|c| c.final_response_tool_names = vec!["deploy".into()]).await;
    let ctx = tp.ctx.with_function_call("call-reject-1").context();
    let deploy_tool = FakeTool::named("deploy");
    let args = obj(json!({"region": "us-central1"}));
    let tool_err = Arc::new(ToolError::Execution(Box::new(NamedToolError {
        name: "deploy".into(),
        source: ToolError::ConfirmationRejected,
    })));

    before_tool(&tp, &ctx, &deploy_tool, &args).await;
    let res = on_tool_error(&tp, &ctx, &deploy_tool, &args, Arc::clone(&tool_err)).await;
    assert!(
        matches!(res, Ok(None)),
        "OnToolErrorCallback() = {res:?}; want None, nil"
    );
    after_tool(
        &tp,
        &ctx,
        &deploy_tool,
        &args,
        Map::new(),
        Some(Arc::clone(&tool_err)),
    )
    .await;
    let rows = finish(&tp).await;
    assert_eq!(
        event_types(&rows),
        [
            EVENT_TOOL_STARTING,
            EVENT_TOOL_COMPLETED,
            EVENT_INVOCATION_COMPLETED
        ],
        "event types"
    );
    let row = only_row_of_type(&rows, EVENT_TOOL_COMPLETED);
    assert_eq!(str_col(&row, "status"), "OK", "status");
    assert!(
        !row.contains_key("error_message"),
        "error_message = {:?}, want none",
        row.get("error_message")
    );
    let want_json = json!({
        "tool": "deploy",
        "result": {"error": tool_err.to_string()},
        "tool_origin": "UNKNOWN",
    })
    .to_string();
    check_json(
        "TOOL_COMPLETED content",
        &json_object_column(&row, "content"),
        &want_json,
    );
    check_json_at(
        &json_object_column(&row, "attributes"),
        r#""call-reject-1""#,
        &["adk", "function_call_id"],
    );
    assert_eq!(
        row.get("span_id"),
        only_row_of_type(&rows, EVENT_TOOL_STARTING).get("span_id"),
        "TOOL_COMPLETED span_id, want TOOL_STARTING span_id"
    );
    assert!(
        row.contains_key(COLUMN_LATENCY_MS),
        "TOOL_COMPLETED row has no latency_ms"
    );
}

#[tokio::test]
async fn test_a2_a_namespaced_sensitive_keys() {
    let new_a2a_event = || {
        let mut ev = event("ev-a2a-redact", "remote-agent");
        ev.response.custom_metadata = obj(json!({
            "a2a:request": {"task": "ping"},
            "a2a:task_id": "task-123",
            "a2a:api_key": "sk-secret",
            "a2a:accessToken": "tok-secret",
            "a2a:temp:scratch": "temp-secret",
            "a2a:note": r#"trace {"api_key":"secret-inline-a2a"}"#,
        }));
        ev
    };

    // default config redacts sensitive keys under a2a: prefix
    {
        let tp = setup_test_plugin().await;
        on_event(&tp, &tp.ctx, new_a2a_event()).await;
        let rows = finish(&tp).await;
        let row = only_row_of_type(&rows, EVENT_A2A_INTERACTION);
        let attrs = json_object_column(&row, "attributes");
        check_json_at(&attrs, r#""task-123""#, &["a2a_metadata", "a2a:task_id"]);
        check_json_at(&attrs, r#""[REDACTED]""#, &["a2a_metadata", "a2a:api_key"]);
        check_json_at(
            &attrs,
            r#""[REDACTED]""#,
            &["a2a_metadata", "a2a:accessToken"],
        );
        check_json_at(
            &attrs,
            r#""[REDACTED]""#,
            &["a2a_metadata", "a2a:temp:scratch"],
        );
        check_json_at(
            &attrs,
            r#""trace {\"api_key\":\"[REDACTED]\"}""#,
            &["a2a_metadata", "a2a:note"],
        );
        let raw_attrs = str_col(&row, "attributes");
        for secret in [
            "sk-secret",
            "tok-secret",
            "temp-secret",
            "secret-inline-a2a",
        ] {
            assert!(
                !raw_attrs.contains(secret),
                "attributes leaked {secret:?}: {raw_attrs}"
            );
        }
    }

    // DisableSensitiveKeyRedaction preserves a2a: values
    {
        let tp = plugin_with(|c| c.disable_sensitive_key_redaction = true).await;
        on_event(&tp, &tp.ctx, new_a2a_event()).await;
        let rows = finish(&tp).await;
        let row = only_row_of_type(&rows, EVENT_A2A_INTERACTION);
        let attrs = json_object_column(&row, "attributes");
        check_json_at(&attrs, r#""sk-secret""#, &["a2a_metadata", "a2a:api_key"]);
        check_json_at(
            &attrs,
            r#""tok-secret""#,
            &["a2a_metadata", "a2a:accessToken"],
        );
        check_json_at(
            &attrs,
            r#""temp-secret""#,
            &["a2a_metadata", "a2a:temp:scratch"],
        );
        check_json_at(
            &attrs,
            r#""trace {\"api_key\":\"secret-inline-a2a\"}""#,
            &["a2a_metadata", "a2a:note"],
        );
    }
}

// The value is a deep self-similar chain of `{"adk_compaction": …}` maps holding no
// compaction keys, so the callback must return promptly and write no EVENT_COMPACTION row.
// parity: adapted: JSON metadata cannot be cyclic (Go's `cyclic = &cyclic`)
#[tokio::test]
async fn test_compaction_cyclic_metadata_does_not_hang() {
    let new_cyclic_event = || {
        let mut cyclic = json!({});
        for _ in 0..100 {
            cyclic = json!({"adk_compaction": cyclic});
        }
        let mut ev = event("ev-cycle-compact", "test-agent");
        ev.response.content = Some(text_content("model", "ok"));
        ev.response.custom_metadata = obj(json!({"adk_compaction": cyclic}));
        ev
    };
    for (name, denylist) in [
        ("default config", Vec::new()),
        (
            "EventEventCompaction denied",
            vec![EVENT_EVENT_COMPACTION.to_owned()],
        ),
    ] {
        let tp = plugin_with(|c| c.event_denylist = denylist).await;
        tokio::time::timeout(
            std::time::Duration::from_millis(250),
            on_event(&tp, &tp.ctx, new_cyclic_event()),
        )
        .await
        .unwrap_or_else(|_| panic!("{name}: OnEventCallback() hung on cyclic compaction value"));
        let rows = finish(&tp).await;
        assert!(
            rows_of_type(&rows, EVENT_EVENT_COMPACTION).is_empty(),
            "{name}: EVENT_COMPACTION rows for cyclic value"
        );
        assert_eq!(
            rows_of_type(&rows, EVENT_AGENT_RESPONSE).len(),
            1,
            "{name}: AGENT_RESPONSE rows"
        );
    }
}

#[tokio::test]
async fn test_custom_metadata_allowlist_gating_on_lifecycle_events() {
    // AGENT_STATE_CHECKPOINT respects CustomMetadataAllowlist
    {
        let state_event = || {
            let mut ev = event("", "state-agent");
            ev.response.custom_metadata = obj(json!({"agent_state": {"step": 1}}));
            ev
        };
        // When allowlist does not include agent_state, AGENT_STATE_CHECKPOINT is not emitted.
        let tp = plugin_with(|c| c.custom_metadata_allowlist = vec!["unrelated_key".into()]).await;
        on_event(&tp, &tp.ctx, state_event()).await;
        let rows = finish(&tp).await;
        assert!(
            rows_of_type(&rows, EVENT_AGENT_STATE_CHECKPOINT).is_empty(),
            "AGENT_STATE_CHECKPOINT rows when not allowlisted"
        );
        // When allowlist includes agent_state, AGENT_STATE_CHECKPOINT is emitted.
        let tp2 = plugin_with(|c| c.custom_metadata_allowlist = vec!["agent_state".into()]).await;
        on_event(&tp2, &tp2.ctx, state_event()).await;
        let rows2 = finish(&tp2).await;
        assert_eq!(
            rows_of_type(&rows2, EVENT_AGENT_STATE_CHECKPOINT).len(),
            1,
            "AGENT_STATE_CHECKPOINT rows when allowlisted"
        );
        let cp_row = only_row_of_type(&rows2, EVENT_AGENT_STATE_CHECKPOINT);
        check_json_at(
            &json_object_column(&cp_row, "content"),
            "1",
            &["agent_state", "step"],
        );
    }

    // EVENT_COMPACTION via metadata respects CustomMetadataAllowlist
    {
        let compaction_event = || {
            let mut ev = event("", "compaction-agent");
            ev.response.custom_metadata = obj(json!({"adk_compaction": {
                "start_timestamp": 1_700_000_000.0,
                "end_timestamp": 1_700_000_100.0,
                "compacted_content": "summary of conversation",
            }}));
            ev
        };
        // When allowlist does not include adk_compaction, EVENT_COMPACTION is not emitted.
        let tp = plugin_with(|c| c.custom_metadata_allowlist = vec!["unrelated_key".into()]).await;
        on_event(&tp, &tp.ctx, compaction_event()).await;
        let rows = finish(&tp).await;
        assert!(
            rows_of_type(&rows, EVENT_EVENT_COMPACTION).is_empty(),
            "EVENT_COMPACTION rows when not allowlisted"
        );
        // When allowlist includes adk_compaction, EVENT_COMPACTION is emitted.
        let tp2 =
            plugin_with(|c| c.custom_metadata_allowlist = vec!["adk_compaction".into()]).await;
        on_event(&tp2, &tp2.ctx, compaction_event()).await;
        let rows2 = finish(&tp2).await;
        assert_eq!(
            rows_of_type(&rows2, EVENT_EVENT_COMPACTION).len(),
            1,
            "EVENT_COMPACTION rows when allowlisted"
        );
    }

    // a2a:* metadata bypasses CustomMetadataAllowlist with sensitive keys redacted
    {
        let tp =
            plugin_with(|c| c.custom_metadata_allowlist = vec!["allowed_user_key".into()]).await;
        let mut ev = event("", "a2a-remote-agent");
        ev.response.custom_metadata = obj(json!({
            "allowed_user_key": "user-visible-val",
            "blocked_user_key": "blocked-val",
            "a2a:request": {"task": "summarize"},
            "a2a:response": {"result": "done"},
            "a2a:api_key": "super-secret-key",
            "a2a:auth_code": "secret-oauth-code",
            "a2a:code_verifier": "secret-verifier",
        }));
        on_event(&tp, &tp.ctx, ev).await;
        let rows = finish(&tp).await;
        let a2a_row = only_row_of_type(&rows, EVENT_A2A_INTERACTION);
        let attrs = json_object_column(&a2a_row, "attributes");
        // a2a:* bypassed allowlist
        check_json_at(
            &attrs,
            r#""summarize""#,
            &["a2a_metadata", "a2a:request", "task"],
        );
        check_json_at(&attrs, r#""[REDACTED]""#, &["a2a_metadata", "a2a:api_key"]);
        check_json_at(
            &attrs,
            r#""[REDACTED]""#,
            &["a2a_metadata", "a2a:auth_code"],
        );
        check_json_at(
            &attrs,
            r#""[REDACTED]""#,
            &["a2a_metadata", "a2a:code_verifier"],
        );
        // custom_metadata only contains allowed_user_key
        check_json_at(
            &attrs,
            r#""user-visible-val""#,
            &["custom_metadata", "allowed_user_key"],
        );
        let raw_attrs = str_col(&a2a_row, "attributes");
        assert!(
            !raw_attrs.contains("blocked-val"),
            "blocked_user_key leaked into attributes: {raw_attrs}"
        );
        for secret in ["super-secret-key", "secret-oauth-code", "secret-verifier"] {
            assert!(
                !raw_attrs.contains(secret),
                "secret {secret:?} leaked into attributes: {raw_attrs}"
            );
        }
    }
}

#[tokio::test]
async fn test_event_compaction_sibling_keys_omitted() {
    let tp = plugin_with(|c| c.custom_metadata_allowlist = vec!["adk_compaction".into()]).await;
    let mut ev = event("", "compaction-agent");
    ev.response.custom_metadata = obj(json!({"adk_compaction": {
        "start_timestamp": 1_700_000_000.0,
        "end_timestamp": 1_700_000_100.0,
        "compacted_content": "conversation summary text",
        "ssn": "123-45-6789",
        "password": "dev-secret-password",
        "internal_tokens": ["tok1", "tok2"],
    }}));
    on_event(&tp, &tp.ctx, ev).await;
    let rows = finish(&tp).await;
    let row = only_row_of_type(&rows, EVENT_EVENT_COMPACTION);
    let content = json_object_column(&row, "content");
    check_json_at(&content, "1.7e+09", &["start_timestamp"]);
    check_json_at(&content, "1.7000001e+09", &["end_timestamp"]);
    check_json_at(
        &content,
        r#""conversation summary text""#,
        &["compacted_content"],
    );
    let raw_content = str_col(&row, "content");
    for forbidden in [
        "ssn",
        "password",
        "internal_tokens",
        "123-45-6789",
        "dev-secret-password",
    ] {
        assert!(
            !raw_content.contains(forbidden),
            "content leaked {forbidden:?}: {raw_content}"
        );
    }
}

#[tokio::test]
async fn test_after_tool_event_denylist_does_not_drop_agent_response() {
    // Using logEvent in afterTool ensures EventDenylist: ["EVENT"] does not drop
    // AGENT_RESPONSE emitted by final response tools.
    let tp = plugin_with(|c| {
        c.final_response_tool_names = vec!["submit_final".into()];
        c.event_denylist = vec!["EVENT".into()];
    })
    .await;
    let submit_tool = FakeTool::named("submit_final");
    let args = obj(json!({"decision": "approved"}));
    let result = obj(json!({"status": "ok", "message": "all done"}));
    let tool_ctx = tp.ctx.context();
    before_tool(&tp, &tool_ctx, &submit_tool, &args).await;
    after_tool(&tp, &tool_ctx, &submit_tool, &args, result, None).await;
    let rows = finish(&tp).await;
    assert!(
        !rows_of_type(&rows, EVENT_AGENT_RESPONSE).is_empty(),
        "expected AGENT_RESPONSE row in afterTool when EventDenylist: ['EVENT'], but got none; events: {:?}",
        event_types(&rows)
    );
}

#[tokio::test]
async fn test_sensitive_keys_standard_redaction_in_compaction_and_a2_a() {
    let tp = plugin_with(|c| c.custom_metadata_allowlist = vec!["adk_compaction".into()]).await;
    let mut ev = event("", "remote-agent");
    ev.response.custom_metadata = obj(json!({
        "adk_compaction": {
            "start_timestamp": 1_700_000_000.0,
            "end_timestamp": 1_700_000_100.0,
            "compacted_content": "login auth_code=MY_AUTH_CODE and code_verifier=MY_VERIFIER_STRING",
        },
        "a2a:request": {
            "note": "exchange auth_code=MY_A2A_CODE and code_verifier=MY_A2A_VERIFIER",
        },
        "a2a:response": {
            "summary": "success with auth_code=MY_A2A_CODE",
        },
    }));
    on_event(&tp, &tp.ctx, ev).await;
    let rows = finish(&tp).await;

    let comp_row = only_row_of_type(&rows, EVENT_EVENT_COMPACTION);
    let comp_content = str_col(&comp_row, "content");
    assert!(
        !comp_content.contains("MY_AUTH_CODE") && !comp_content.contains("MY_VERIFIER_STRING"),
        "EVENT_COMPACTION content leaked sensitive keys: {comp_content}"
    );
    assert!(
        comp_content.contains("[REDACTED]"),
        "EVENT_COMPACTION content missing [REDACTED]: {comp_content}"
    );

    let a2a_row = only_row_of_type(&rows, EVENT_A2A_INTERACTION);
    let attrs = json_object_column(&a2a_row, "attributes");
    let raw_attrs = str_col(&a2a_row, "attributes");
    assert!(
        !raw_attrs.contains("MY_A2A_CODE") && !raw_attrs.contains("MY_A2A_VERIFIER"),
        "A2A_INTERACTION attributes leaked sensitive keys: {raw_attrs}"
    );
    check_json_at(
        &attrs,
        r#""exchange auth_code=[REDACTED] and code_verifier=[REDACTED]""#,
        &["a2a_metadata", "a2a:request", "note"],
    );
}
