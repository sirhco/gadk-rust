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

//! Port of ADK-Go `plugin/agentanalytics/doc_test.go` (ADK-Go #2176): the runtime pins of the
//! behaviours that the package godoc (`doc.go`) documents. The tests drive the plugin's callbacks
//! with one [`Mc`] as the 7b-3a direct callback ports do; Go's `callbackErrors` is each
//! callback's result unwrapped.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::Duration;

use chrono::{TimeZone, Utc};
use gadk_agent::Tool;
use gadk_genai::{
    Content, FunctionCall, FunctionDeclaration, FunctionResponse,
    GenerateContentResponseUsageMetadata, MediaModality, ModalityTokenCount, Part, Schema, Type,
};
use gadk_model::{LlmRequest, LlmResponse, ModelError};
use gadk_session::{Event, EventActions, EventCompaction, NodeInfo, RequestInput};
use parking_lot::Mutex;
use pretty_assertions::assert_eq;
use serde_json::{Map, Value, json};

use super::plugin_agentanalytics_batch_processor::support::content_parts_of;
use super::plugin_agentanalytics_bigquery_agent_analytics_plugin::support::{
    FakeTool, Mc, TestPlugin, check_json, check_json_at, event_types, json_object_column, lookup,
    new_test_plugin_with_config, only_row_of_type, rows_of_type, setup_test_plugin, str_col,
    text_content, until_event_type,
};
use crate::bigquery::fake::FakeOffloader;
use crate::config::{Config, ContentFormatter};
use crate::event_types::*;
use crate::events::has_actions_compaction;
use crate::formatter::{TRUNCATION_SUFFIX, smart_truncate};
use crate::plugin::rows::llm_request_attributes;
use crate::redaction::{COLUMN_ATTRIBUTES, COLUMN_CONTENT, REDACTED_VALUE};
use crate::row::{Row, RowExt};

// ---------------------------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------------------------

/// Go `p.BeforeRunCallback()(mCtx)`, which must not fail.
async fn before_run(tp: &TestPlugin, m: &Mc) {
    (tp.plugin.before_run_callback().unwrap())(m.invocation())
        .await
        .unwrap_or_else(|e| panic!("BeforeRunCallback() error: {e}"));
}

/// Go `p.BeforeAgentCallback()(mCtx)`, which must not fail.
async fn before_agent(tp: &TestPlugin, m: &Mc) {
    (tp.plugin.before_agent_callback().unwrap())(m.context())
        .await
        .unwrap_or_else(|e| panic!("BeforeAgentCallback() error: {e}"));
}

/// Go `p.BeforeModelCallback()(mCtx, req)`, which must not fail.
async fn before_model(tp: &TestPlugin, m: &Mc, req: LlmRequest) {
    (tp.plugin.before_model_callback().unwrap())(m.context(), req)
        .await
        .1
        .unwrap_or_else(|e| panic!("BeforeModelCallback() error: {e}"));
}

/// Go `p.AfterModelCallback()(mCtx, res, nil)`, which must not fail.
async fn after_model(tp: &TestPlugin, m: &Mc, res: LlmResponse) {
    (tp.plugin.after_model_callback().unwrap())(m.context(), res, None)
        .await
        .1
        .unwrap_or_else(|e| panic!("AfterModelCallback() error: {e}"));
}

/// Go `p.OnEventCallback()(mCtx, ev)`, which must not fail.
async fn on_event(tp: &TestPlugin, m: &Mc, ev: Event) {
    (tp.plugin.on_event_callback().unwrap())(m.invocation(), ev)
        .await
        .unwrap_or_else(|e| panic!("OnEventCallback() error: {e}"));
}

/// Go `p.AfterRunCallback()(mCtx)`.
async fn after_run(tp: &TestPlugin, m: &Mc) {
    (tp.plugin.after_run_callback().unwrap())(m.invocation()).await;
}

/// Go's `mCtx` of `setupTestPlugin`.
fn mc(tp: &TestPlugin) -> Mc {
    Mc::of(&tp.ctx)
}

/// The rows of the invocation, once `INVOCATION_COMPLETED` is written.
async fn completed_rows(tp: &TestPlugin) -> Vec<Row> {
    tp.collect_rows(until_event_type(EVENT_INVOCATION_COMPLETED))
        .await
}

/// A JSON object literal.
fn obj(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        other => panic!("{other} is not an object"),
    }
}

/// Go `&genai.Part{FunctionCall: fc}`.
fn call_part(fc: FunctionCall) -> Part {
    Part {
        function_call: Some(fc),
        ..Part::default()
    }
}

/// Go `&genai.Part{FunctionResponse: fr}`.
fn response_part(fr: FunctionResponse) -> Part {
    Part {
        function_response: Some(fr),
        ..Part::default()
    }
}

/// Go `&genai.Part{FunctionCall: &genai.FunctionCall{Name: name}}`.
fn named_call(name: &str) -> Part {
    call_part(FunctionCall {
        name: name.into(),
        ..FunctionCall::default()
    })
}

/// Go `&genai.Part{FunctionResponse: &genai.FunctionResponse{Name: name}}`.
fn named_response(name: &str) -> Part {
    response_part(FunctionResponse {
        name: name.into(),
        ..FunctionResponse::default()
    })
}

/// Go `&genai.Content{Role: "model", Parts: parts}`.
fn model_content(parts: Vec<Part>) -> Content {
    Content {
        role: "model".into(),
        parts,
    }
}

/// Go `responseTextParts(n, size, r)` (`json_formatter_test.go`): `n` text parts of `size`
/// copies of `r`.
fn response_text_parts(n: usize, size: usize, r: &str) -> Vec<Part> {
    (0..n).map(|_| Part::from_text(r.repeat(size))).collect()
}

/// Go `responseTextPiece(text)`: how a response summary shows a text part.
fn response_text_piece(text: &str) -> String {
    format!("text: '{text}'")
}

/// Go `joinedResponsePieces(piece, n)`: `n` copies of `piece` joined as a response summary.
fn joined_response_pieces(piece: &str, n: usize) -> String {
    vec![piece; n].join(" | ")
}

/// Go's `CustomMetadata{"adk_compaction": map[string]any{…, "compacted_content": content}}`
/// as gadk's JSON custom metadata (the content as its JSON).
fn compaction_metadata(content: &Content) -> Map<String, Value> {
    obj(json!({"adk_compaction": {
        "start_timestamp": 1_700_000_000.0,
        "end_timestamp": 1_700_000_100.0,
        "compacted_content": serde_json::to_value(content).unwrap(),
    }}))
}

/// An event of `author` with `id`.
fn event(id: &str, author: &str) -> Event {
    Event {
        id: id.into(),
        author: author.into(),
        ..Event::default()
    }
}

/// A string field of a JSON column (Go `jsonObjectColumn(t, row, col)[key].(string)`).
fn json_str(row: &Row, column: &str, key: &str) -> String {
    json_object_column(row, column)
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// Go `row["is_truncated"]`.
fn is_truncated(row: &Row) -> Option<bool> {
    row.bool("is_truncated")
}

// ---------------------------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------------------------

// parity: skipped TestDocGo_ParityBulletsReferenceExistingTests: parses doc.go's Go package comment with go/parser and checks that each cited TestName resolves to a Go test function of the package (a Go-source check with no Rust counterpart)

#[tokio::test]
async fn test_doc_parity_error_traceback_and_unclosed_span_synthetic_error() {
    // recorded error populates content.error_traceback with Error() text on AGENT_ERROR and
    // INVOCATION_ERROR
    {
        let tp = setup_test_plugin().await;
        let m = mc(&tp);
        before_run(&tp, &m).await;
        before_agent(&tp, &m).await;
        before_model(
            &tp,
            &m,
            LlmRequest {
                model: "gemini-test".into(),
                ..LlmRequest::default()
            },
        )
        .await;
        let res = (tp.plugin.on_model_error_callback().unwrap())(
            m.context(),
            LlmRequest {
                model: "gemini-test".into(),
                ..LlmRequest::default()
            },
            Arc::new(ModelError::Other("model exploded".into())),
        )
        .await;
        assert!(
            matches!(res, Ok(None)),
            "OnModelErrorCallback() = {res:?}; want nil, nil"
        );
        after_run(&tp, &m).await;

        let rows = tp
            .collect_rows(until_event_type(EVENT_INVOCATION_ERROR))
            .await;
        let agent_err_row = only_row_of_type(&rows, EVENT_AGENT_ERROR);
        assert_eq!(
            str_col(&agent_err_row, "error_message"),
            "model exploded",
            "AGENT_ERROR error_message"
        );
        check_json(
            "AGENT_ERROR content",
            &json_object_column(&agent_err_row, COLUMN_CONTENT),
            r#"{"error_traceback":"model exploded"}"#,
        )
        .unwrap_or_else(|e| panic!("{e}"));

        let inv_err_row = only_row_of_type(&rows, EVENT_INVOCATION_ERROR);
        assert_eq!(
            str_col(&inv_err_row, "error_message"),
            "model exploded",
            "INVOCATION_ERROR error_message"
        );
        check_json(
            "INVOCATION_ERROR content",
            &json_object_column(&inv_err_row, COLUMN_CONTENT),
            r#"{"error_traceback":"model exploded"}"#,
        )
        .unwrap_or_else(|e| panic!("{e}"));
    }

    // unclosed agent span with no recorded error synthesizes error_message and
    // content.error_traceback
    {
        let tp = setup_test_plugin().await;
        let m = mc(&tp);
        before_run(&tp, &m).await;
        before_agent(&tp, &m).await;
        after_run(&tp, &m).await;

        const WANT_MSG: &str = r#"agent "test-agent" execution failed before completion"#;
        const WANT_CONTENT: &str =
            r#"{"error_traceback":"agent \"test-agent\" execution failed before completion"}"#;
        let rows = tp
            .collect_rows(until_event_type(EVENT_INVOCATION_ERROR))
            .await;
        let agent_err_row = only_row_of_type(&rows, EVENT_AGENT_ERROR);
        assert_eq!(
            str_col(&agent_err_row, "status"),
            "ERROR",
            "AGENT_ERROR status"
        );
        assert_eq!(
            str_col(&agent_err_row, "error_message"),
            WANT_MSG,
            "AGENT_ERROR error_message"
        );
        check_json(
            "AGENT_ERROR content",
            &json_object_column(&agent_err_row, COLUMN_CONTENT),
            WANT_CONTENT,
        )
        .unwrap_or_else(|e| panic!("{e}"));

        let inv_err_row = only_row_of_type(&rows, EVENT_INVOCATION_ERROR);
        assert_eq!(
            str_col(&inv_err_row, "error_message"),
            WANT_MSG,
            "INVOCATION_ERROR error_message"
        );
        check_json(
            "INVOCATION_ERROR content",
            &json_object_column(&inv_err_row, COLUMN_CONTENT),
            WANT_CONTENT,
        )
        .unwrap_or_else(|e| panic!("{e}"));
    }
}

#[tokio::test]
async fn test_doc_parity_paused_long_running_tool_writes_two_tool_completed_rows() {
    let tp = setup_test_plugin().await;
    let m = mc(&tp);
    let tool_ctx = Mc::of(&tp.ctx.with_function_call("call-lr-1"));
    let deploy_tool: Arc<dyn Tool> = Arc::new(FakeTool {
        name: "deploy_job".into(),
        long_running: true,
        ..FakeTool::default()
    });
    let args = obj(json!({"region": "us-central1"}));

    (tp.plugin.before_tool_callback().unwrap())(
        tool_ctx.context(),
        Arc::clone(&deploy_tool),
        args.clone(),
    )
    .await
    .unwrap_or_else(|e| panic!("BeforeToolCallback() error: {e}"));
    (tp.plugin.after_tool_callback().unwrap())(
        tool_ctx.context(),
        Arc::clone(&deploy_tool),
        args.clone(),
        obj(json!({"status": "pending", "ticket": "t-1"})),
        None,
    )
    .await
    .unwrap_or_else(|e| panic!("AfterToolCallback() error: {e}"));
    let mut pause = event("ev-lr-pause", "test-agent");
    pause.long_running_tool_ids = vec!["call-lr-1".into()];
    pause.response.content = Some(model_content(vec![call_part(FunctionCall {
        id: "call-lr-1".into(),
        name: "deploy_job".into(),
        args: args.clone(),
        ..FunctionCall::default()
    })]));
    on_event(&tp, &m, pause).await;
    (tp.plugin.on_user_message_callback().unwrap())(
        m.invocation(),
        Content {
            role: "user".into(),
            parts: vec![response_part(FunctionResponse {
                id: "call-lr-1".into(),
                name: "deploy_job".into(),
                response: obj(json!({"status": "done", "job_id": "j-99"})),
                ..FunctionResponse::default()
            })],
        },
    )
    .await
    .unwrap_or_else(|e| panic!("OnUserMessageCallback() error: {e}"));
    after_run(&tp, &m).await;

    let rows = completed_rows(&tp).await;
    let want_types = vec![
        EVENT_TOOL_STARTING,
        EVENT_TOOL_COMPLETED,
        EVENT_TOOL_PAUSED,
        EVENT_USER_MESSAGE_RECEIVED,
        EVENT_TOOL_COMPLETED,
        EVENT_INVOCATION_COMPLETED,
    ];
    assert_eq!(want_types, event_types(&rows), "event types mismatch");

    let paused_row = only_row_of_type(&rows, EVENT_TOOL_PAUSED);
    let paused_attrs = json_object_column(&paused_row, COLUMN_ATTRIBUTES);
    check_json_at(&paused_attrs, r#""tool""#, &["adk", "pause_kind"]);
    check_json_at(
        &paused_attrs,
        r#""call-lr-1""#,
        &["adk", "function_call_id"],
    );

    let completed = rows_of_type(&rows, EVENT_TOOL_COMPLETED);
    assert_eq!(completed.len(), 2, "TOOL_COMPLETED rows");
    let initial_attrs = json_object_column(&completed[0], COLUMN_ATTRIBUTES);
    check_json_at(
        &initial_attrs,
        r#""call-lr-1""#,
        &["adk", "function_call_id"],
    );
    if let Some(v) = lookup(&initial_attrs, &["adk", "pause_kind"]) {
        panic!("initial TOOL_COMPLETED adk.pause_kind = {v}, want absent");
    }
    check_json_at(
        &json_object_column(&completed[0], COLUMN_CONTENT),
        r#""pending""#,
        &["result", "status"],
    );

    let resumed_attrs = json_object_column(&completed[1], COLUMN_ATTRIBUTES);
    check_json_at(
        &resumed_attrs,
        r#""call-lr-1""#,
        &["adk", "function_call_id"],
    );
    check_json_at(&resumed_attrs, r#""tool""#, &["adk", "pause_kind"]);
    check_json_at(
        &json_object_column(&completed[1], COLUMN_CONTENT),
        r#""done""#,
        &["result", "status"],
    );
}

// parity: adapted: Go probes hasActionsCompaction with an anonymous struct and sets EventActions.Compaction by reflection; gadk's EventActions always has a typed compaction, checked directly and sent through OnEventCallback
#[tokio::test]
async fn test_doc_parity_agent_response_excludes_long_running_tools_and_compaction() {
    let tp = setup_test_plugin().await;
    let m = mc(&tp);

    // Non-empty LongRunningToolIDs suppresses AGENT_RESPONSE even when text is present.
    let mut lr = event("ev-lr-text", "test-agent");
    lr.long_running_tool_ids = vec!["call-wait-1".into()];
    lr.response.content = Some(text_content("model", "waiting for external job"));
    on_event(&tp, &m, lr).await;

    // Actions.Compaction suppresses AGENT_RESPONSE even when text is present.
    let compaction = EventCompaction {
        start_timestamp: Utc.with_ymd_and_hms(2026, 10, 10, 1, 0, 0).unwrap(),
        end_timestamp: Utc.with_ymd_and_hms(2026, 10, 10, 1, 5, 0).unwrap(),
        compacted_content: Some(text_content("model", "compacted summary")),
        excluded_events: Vec::new(),
    };
    assert!(
        has_actions_compaction(&EventActions {
            compaction: Some(compaction.clone()),
            ..EventActions::default()
        }),
        "hasActionsCompaction() = false, want true when Compaction is non-nil"
    );
    assert!(
        !has_actions_compaction(&EventActions::default()),
        "hasActionsCompaction() = true, want false when Compaction is nil"
    );

    let mut ev_compact = event("ev-compact-text", "test-agent");
    ev_compact.response.content = Some(text_content("model", "compacted summary"));
    ev_compact.actions.compaction = Some(compaction);
    on_event(&tp, &m, ev_compact).await;
    after_run(&tp, &m).await;

    let rows = completed_rows(&tp).await;
    let n = rows_of_type(&rows, EVENT_AGENT_RESPONSE).len();
    assert_eq!(
        n,
        0,
        "AGENT_RESPONSE rows; event types: {:?}",
        event_types(&rows)
    );
    only_row_of_type(&rows, EVENT_EVENT_COMPACTION);
}

#[tokio::test]
async fn test_doc_parity_offloaded_single_text_llm_response_writes_empty_response_string() {
    let off: Arc<FakeOffloader> = Arc::new(FakeOffloader::with_bucket("test-bucket"));
    let tp = new_test_plugin_with_config(|c| {
        c.gcs_offloader = Some(off.clone());
        c.batch_flush_intv = Duration::from_millis(10);
        c.max_inline_text_size = 20;
    })
    .await;
    let m = mc(&tp);
    let long_resp = format!("OFFLOADED-SINGLE-{}", "Z".repeat(80));
    after_model(
        &tp,
        &m,
        LlmResponse {
            content: Some(text_content("model", &long_resp)),
            ..LlmResponse::default()
        },
    )
    .await;
    after_run(&tp, &m).await;

    let rows = completed_rows(&tp).await;
    let row = only_row_of_type(&rows, EVENT_LLM_RESPONSE);
    check_json(
        "single offloaded LLM_RESPONSE content",
        &json_object_column(&row, COLUMN_CONTENT),
        r#"{"response":""}"#,
    )
    .unwrap_or_else(|e| panic!("{e}"));

    let parts = content_parts_of(&row);
    assert_eq!(parts.len(), 1, "content_parts");
    assert_eq!(
        parts[0].str("storage_mode"),
        Some("GCS_REFERENCE"),
        "storage_mode"
    );
    let uri = parts[0].str("uri").unwrap_or_default().to_owned();
    assert!(uri.starts_with("gs://"), "uri = {uri:?}, want gs:// URI");
    let obj_ref_uri = parts[0].map("object_ref").and_then(|r| r.str("uri"));
    assert_eq!(
        obj_ref_uri,
        Some(uri.as_str()),
        "object_ref = {:?}, want map with uri {uri:?}",
        parts[0].get("object_ref")
    );
    let preview = parts[0].str("text").unwrap_or_default();
    assert!(
        preview.starts_with("OFFLOADED-SINGLE-"),
        "preview text = {preview:?}, want prefix OFFLOADED-SINGLE-"
    );
}

#[tokio::test]
async fn test_doc_parity_state_delta_trims_leading_whitespace_before_temp_prefix() {
    let tp = setup_test_plugin().await;
    let m = mc(&tp);

    // Leading whitespace before "temp:" must still be scrubbed; temp-only delta emits no row.
    let mut temp_only = event("ev-ws-temp-only", "test-agent");
    temp_only.actions.state_delta = Some(obj(json!({"  \ttemp:scratch": "ephemeral"})));
    on_event(&tp, &m, temp_only).await;

    // Mixed delta strips whitespace-padded "temp:" key and preserves durable key.
    let mut mixed = event("ev-ws-temp-mixed", "test-agent");
    mixed.actions.state_delta = Some(obj(
        json!({"  temp:ephemeral": "drop-me", "durable": "keep-me"}),
    ));
    on_event(&tp, &m, mixed).await;
    after_run(&tp, &m).await;

    let rows = completed_rows(&tp).await;
    let row = only_row_of_type(&rows, EVENT_STATE_DELTA);
    check_json_at(
        &json_object_column(&row, COLUMN_ATTRIBUTES),
        r#"{"durable":"keep-me"}"#,
        &["state_delta"],
    );
}

#[tokio::test]
async fn test_doc_parity_hitl_input_request_from_requested_input() {
    let tp = setup_test_plugin().await;
    let m = mc(&tp);
    let mut ev = event("ev-req-input-only", "workflow-agent");
    ev.requested_input = Some(RequestInput {
        interrupt_id: "int-only-1".into(),
        message: "Provide target region".into(),
        ..RequestInput::default()
    });
    on_event(&tp, &m, ev).await;
    after_run(&tp, &m).await;

    let rows = completed_rows(&tp).await;
    let row = only_row_of_type(&rows, EVENT_HITL_INPUT_REQUEST);
    check_json(
        "HITL_INPUT_REQUEST content",
        &json_object_column(&row, COLUMN_CONTENT),
        r#"{"tool":"adk_request_input","args":{"interruptId":"int-only-1","message":"Provide target region"},"requested_input":{"interruptId":"int-only-1","message":"Provide target region"}}"#,
    )
    .unwrap_or_else(|e| panic!("{e}"));
}

// parity: adapted: Go's closing reflect.TypeFor check that session.Event / EventActions define no RenderUIWidgets or RewindBeforeInvocationID field has no Rust counterpart (gadk's types have neither field); the row checks are kept
#[tokio::test]
async fn test_doc_parity_adk_node_attributes_and_omissions() {
    let tp = setup_test_plugin().await;
    let m = mc(&tp);

    // Non-workflow event (NodeInfo == nil) must omit attributes.adk.node completely, and
    // render_ui_widgets / rewind_before_invocation_id must never appear.
    let mut plain = event("ev-no-node", "test-agent");
    plain.response.content = Some(text_content("model", "plain answer"));
    on_event(&tp, &m, plain).await;

    // Workflow node event with MessageAsOutput and SkipSummarization must record
    // adk.node.message_as_output = true and adk.skip_summarization = true.
    let mut node = event("ev-msg-as-output", "workflow-node");
    node.node_info = Some(NodeInfo {
        path: "wf@0/msg@1".into(),
        message_as_output: true,
        ..NodeInfo::default()
    });
    node.actions.skip_summarization = true;
    node.response.content = Some(text_content("model", "node text answer"));
    on_event(&tp, &m, node).await;
    after_run(&tp, &m).await;

    let rows = completed_rows(&tp).await;
    let resp_rows = rows_of_type(&rows, EVENT_AGENT_RESPONSE);
    assert_eq!(resp_rows.len(), 2, "AGENT_RESPONSE rows");

    let plain_attrs = json_object_column(&resp_rows[0], COLUMN_ATTRIBUTES);
    if let Some(v) = lookup(&plain_attrs, &["adk", "node"]) {
        panic!("attributes.adk.node = {v} when NodeInfo == nil, want absent");
    }
    if let Some(v) = lookup(&plain_attrs, &["adk", "skip_summarization"]) {
        panic!("attributes.adk.skip_summarization = {v} when unset, want absent");
    }

    let msg_out_attrs = json_object_column(&resp_rows[1], COLUMN_ATTRIBUTES);
    check_json_at(
        &msg_out_attrs,
        r#"{"path":"wf@0/msg@1","run_id":"1","parent_run_id":"0","message_as_output":true}"#,
        &["adk", "node"],
    );
    check_json_at(&msg_out_attrs, "true", &["adk", "skip_summarization"]);

    for row in &rows {
        let attrs = json_object_column(row, COLUMN_ATTRIBUTES);
        for omitted_key in ["render_ui_widgets", "rewind_before_invocation_id"] {
            if let Some(v) = lookup(&attrs, &["adk", omitted_key]) {
                panic!(
                    "{} attributes.adk.{omitted_key} = {v}, want absent",
                    str_col(row, "event_type")
                );
            }
        }
    }
}

#[tokio::test]
async fn test_doc_parity_usage_metadata_and_content_usage_shape() {
    let tp = setup_test_plugin().await;
    let m = mc(&tp);

    // 1. Non-zero optional counts + modality breakdown arrays: optional usage_metadata and
    // content.usage fields are emitted, while modality details and nil fields are omitted.
    let text_count = |n| ModalityTokenCount {
        modality: Some(MediaModality::Text),
        token_count: n,
    };
    after_model(
        &tp,
        &m,
        LlmResponse {
            content: Some(text_content("model", "detailed")),
            usage_metadata: Some(GenerateContentResponseUsageMetadata {
                prompt_token_count: 12,
                candidates_token_count: 8,
                total_token_count: 29,
                thoughts_token_count: 4,
                cached_content_token_count: 3,
                tool_use_prompt_token_count: 2,
                prompt_tokens_details: vec![text_count(12)],
                candidates_tokens_details: vec![text_count(8)],
                ..GenerateContentResponseUsageMetadata::default()
            }),
            ..LlmResponse::default()
        },
    )
    .await;

    // 2. Zero/omitted counts: content.usage writes integer 0 for core counts and omits
    // thoughts/cached; usage_metadata omits zero optional keys instead of writing null.
    after_model(
        &tp,
        &m,
        LlmResponse {
            content: Some(text_content("model", "zeros")),
            usage_metadata: Some(GenerateContentResponseUsageMetadata::default()),
            ..LlmResponse::default()
        },
    )
    .await;
    after_run(&tp, &m).await;

    let rows = completed_rows(&tp).await;
    let resp_rows = rows_of_type(&rows, EVENT_LLM_RESPONSE);
    assert_eq!(resp_rows.len(), 2, "LLM_RESPONSE rows");

    // Row 1 checks: full positive optional fields, no modality breakdowns.
    let c1 = json_object_column(&resp_rows[0], COLUMN_CONTENT);
    check_json_at(
        &c1,
        r#"{"prompt":12,"completion":8,"total":29,"thoughts":4,"cached":3}"#,
        &["usage"],
    );
    let a1 = json_object_column(&resp_rows[0], COLUMN_ATTRIBUTES);
    check_json_at(
        &a1,
        r#"{"prompt_token_count":12,"candidates_token_count":8,"total_token_count":29,"thoughts_token_count":4,"cached_content_token_count":3,"tool_use_prompt_token_count":2}"#,
        &["usage_metadata"],
    );

    // Row 2 checks: integer 0 counts and strict omission of zero optional fields / nulls.
    let c2 = json_object_column(&resp_rows[1], COLUMN_CONTENT);
    check_json_at(&c2, r#"{"prompt":0,"completion":0,"total":0}"#, &["usage"]);
    let a2 = json_object_column(&resp_rows[1], COLUMN_ATTRIBUTES);
    check_json_at(
        &a2,
        r#"{"prompt_token_count":0,"candidates_token_count":0,"total_token_count":0}"#,
        &["usage_metadata"],
    );
}

// parity: adapted: LlmRequest.tools holds each tool as an `Arc<dyn Tool>` behind `dyn Any` (as the 7b-3a TestLLMRequestAttributes_Tools port); Go's internal smartTruncate(tools, -1, true) is gadk's smart_truncate (redaction on)
#[test]
fn test_doc_parity_llm_request_tool_schema_camel_case_and_deterministic_order() {
    fn packed(t: FakeTool) -> Arc<dyn std::any::Any + Send + Sync> {
        let t: Arc<dyn Tool> = Arc::new(t);
        Arc::new(t)
    }
    let mut req = LlmRequest::default();
    req.tools.insert(
        "zebra".into(),
        packed(FakeTool {
            name: "zebra".into(),
            description: "Last alphabetically".into(),
            decl: Some(FunctionDeclaration {
                name: "zebra".into(),
                parameters: Some(Schema {
                    r#type: Some(Type::Object),
                    properties: [
                        (
                            "tags".to_owned(),
                            Schema {
                                r#type: Some(Type::Array),
                                max_items: Some(3),
                                items: Some(Box::new(Schema {
                                    r#type: Some(Type::String),
                                    ..Schema::default()
                                })),
                                ..Schema::default()
                            },
                        ),
                        (
                            "target".to_owned(),
                            Schema {
                                any_of: vec![
                                    Schema {
                                        r#type: Some(Type::String),
                                        ..Schema::default()
                                    },
                                    Schema {
                                        r#type: Some(Type::Integer),
                                        ..Schema::default()
                                    },
                                ],
                                ..Schema::default()
                            },
                        ),
                    ]
                    .into_iter()
                    .collect(),
                    ..Schema::default()
                }),
                ..FunctionDeclaration::default()
            }),
            ..FakeTool::default()
        }),
    );
    req.tools.insert(
        "alpha".into(),
        packed(FakeTool {
            name: "alpha".into(),
            description: "First alphabetically".into(),
            ..FakeTool::default()
        }),
    );
    req.tools.insert(
        "middle".into(),
        packed(FakeTool {
            name: "middle".into(),
            description: "Second alphabetically".into(),
            ..FakeTool::default()
        }),
    );

    let mut prev_raw = String::new();
    for run in 0..10 {
        let attrs = llm_request_attributes(&req);
        let tools = attrs
            .get("tools")
            .unwrap_or_else(|| panic!("llmRequestAttributes() missing tools"));
        let (b, _) =
            smart_truncate(tools, -1).unwrap_or_else(|e| panic!("run {run} smartTruncate(): {e}"));
        let got_raw = String::from_utf8(b).unwrap();
        assert!(
            prev_raw.is_empty() || got_raw == prev_raw,
            "run {run} tool order drifted:\ngot:  {got_raw}\nwant: {prev_raw}"
        );
        prev_raw = got_raw.clone();

        let parsed: Vec<Value> = serde_json::from_str(&got_raw)
            .unwrap_or_else(|e| panic!("json.Unmarshal({got_raw}): {e}"));
        let names: Vec<&str> = parsed
            .iter()
            .map(|item| item["name"].as_str().unwrap_or_default())
            .collect();
        assert_eq!(
            vec!["alpha", "middle", "zebra"],
            names,
            "tool ordering mismatch"
        );

        let props = &parsed[2]["parameters"]["properties"];
        let tags = &props["tags"];
        assert_eq!(
            tags.get("maxItems").and_then(Value::as_f64),
            Some(3.0),
            "tags.maxItems; raw={tags}"
        );
        assert!(
            tags.get("max_items").is_none(),
            "unexpected snake_case max_items in {tags}"
        );
        let target = &props["target"];
        assert_eq!(
            target.get("anyOf").and_then(Value::as_array).map(Vec::len),
            Some(2),
            "target.anyOf = {:?}, want camelCase 2-element slice",
            target.get("anyOf")
        );
        assert!(
            target.get("any_of").is_none(),
            "unexpected snake_case any_of in {target}"
        );
    }
}

#[tokio::test]
async fn test_doc_parity_content_parts_function_call_args_truncated() {
    let tp = new_test_plugin_with_config(|c| c.max_content_len = 30).await;
    let m = mc(&tp);
    after_model(
        &tp,
        &m,
        LlmResponse {
            content: Some(model_content(vec![call_part(FunctionCall {
                id: "call-trunc-1".into(),
                name: "search_docs".into(),
                args: obj(json!({
                    "api_key": "sk-super-secret",
                    "query": "word-".repeat(20),
                })),
                ..FunctionCall::default()
            })])),
            ..LlmResponse::default()
        },
    )
    .await;
    after_run(&tp, &m).await;

    let rows = completed_rows(&tp).await;
    let row = only_row_of_type(&rows, EVENT_LLM_RESPONSE);
    assert_eq!(
        is_truncated(&row),
        Some(true),
        "is_truncated for oversized function_call args"
    );
    let parts = content_parts_of(&row);
    assert_eq!(parts.len(), 1, "content_parts");
    let attrs_str = parts[0].str("part_attributes").unwrap_or_default();
    assert!(
        !attrs_str.contains("sk-super-secret"),
        "part_attributes leaked secret: {attrs_str}"
    );
    let attrs: Value = serde_json::from_str(attrs_str)
        .unwrap_or_else(|e| panic!("json.Unmarshal(part_attributes): {e}"));
    assert_eq!(attrs["function_name"], "search_docs", "function_name");
    let query_arg = lookup(&attrs, &["function_call", "args", "query"])
        .unwrap_or_else(|| panic!("missing function_call.args.query in {attrs_str}"));
    let s = query_arg.as_str().unwrap_or_default();
    assert!(
        s.ends_with("...[TRUNCATED]"),
        "function_call.args.query = {s:?}, want ...[TRUNCATED] suffix"
    );
}

// parity: adapted: gadk's ContentFormatter receives the payload's JSON (an LLMRequest as Go's json.Marshal of model.LLMRequest, an LLMResponse as its JSON), so "received *model.LLMRequest / *model.LLMResponse" is the JSON of that type
#[tokio::test]
async fn test_doc_parity_content_formatter_receives_typed_model_payloads_and_skips_nil_content() {
    #[derive(Default)]
    struct Seen {
        calls: Vec<String>,
        req_type: bool,
        resp_type: bool,
    }
    let seen = Arc::new(Mutex::new(Seen::default()));
    let record = Arc::clone(&seen);
    let formatter: ContentFormatter = Arc::new(move |payload: &Value, event_type: &str| {
        let mut s = record.lock();
        s.calls.push(event_type.to_owned());
        match event_type {
            EVENT_LLM_REQUEST => {
                let model = payload.get("Model").and_then(Value::as_str);
                s.req_type = model.is_some();
                if let Some(model) = model {
                    return Ok(json!({"custom_req_model": model}));
                }
            }
            EVENT_LLM_RESPONSE => {
                let content = payload.get("content");
                s.resp_type = payload.is_object();
                if content.is_some_and(|c| !c.is_null()) {
                    return Ok(json!({"custom_resp": "formatted"}));
                }
            }
            _ => {}
        }
        Ok(payload.clone())
    });
    let tp =
        new_test_plugin_with_config(|c: &mut Config| c.content_formatter = Some(formatter)).await;
    let m = mc(&tp);

    before_run(&tp, &m).await;
    before_model(
        &tp,
        &m,
        LlmRequest {
            model: "gemini-custom".into(),
            contents: vec![text_content("user", "hi")],
            ..LlmRequest::default()
        },
    )
    .await;
    after_model(
        &tp,
        &m,
        LlmResponse {
            content: Some(text_content("model", "hello")),
            ..LlmResponse::default()
        },
    )
    .await;
    after_run(&tp, &m).await;

    let rows = completed_rows(&tp).await;
    let s = seen.lock();
    assert!(
        s.req_type,
        "ContentFormatter did not receive *model.LLMRequest on EventLLMRequest"
    );
    assert!(
        s.resp_type,
        "ContentFormatter did not receive *model.LLMResponse on EventLLMResponse"
    );
    assert_eq!(
        vec![EVENT_LLM_REQUEST, EVENT_LLM_RESPONSE],
        s.calls,
        "ContentFormatter calls mismatch (nil-content rows like INVOCATION_STARTING/INVOCATION_COMPLETED must be skipped)"
    );
    check_json(
        "formatted LLM_REQUEST content",
        &json_object_column(&only_row_of_type(&rows, EVENT_LLM_REQUEST), COLUMN_CONTENT),
        r#"{"custom_req_model":"gemini-custom"}"#,
    )
    .unwrap_or_else(|e| panic!("{e}"));
    check_json(
        "formatted LLM_RESPONSE content",
        &json_object_column(&only_row_of_type(&rows, EVENT_LLM_RESPONSE), COLUMN_CONTENT),
        r#"{"custom_resp":"formatted"}"#,
    )
    .unwrap_or_else(|e| panic!("{e}"));
}

#[test]
fn test_doc_parity_default_config_batch_size_and_retry_delay() {
    let cfg = Config::default();
    assert_eq!(cfg.batch_size, 100, "DefaultConfig().BatchSize");
    assert_eq!(
        cfg.retry_config.initial_delay,
        Duration::from_millis(100),
        "DefaultConfig().RetryConfig.InitialDelay"
    );

    let applied = Config::zero().apply_defaults();
    assert_eq!(
        applied.batch_size, 100,
        "applyDefaults(Config{{}}).BatchSize"
    );
    assert_eq!(
        applied.retry_config.initial_delay,
        Duration::from_millis(100),
        "applyDefaults(Config{{}}).RetryConfig.InitialDelay"
    );
}

// parity: adapted: gadk custom metadata is JSON, so the adk_compaction compacted_content *genai.Content is its JSON; the CaptureFullMessageContent and MaxContentLen -1 subtests (same checks) share one loop
#[tokio::test]
#[ignore = "8a bug: response summaries neither cap function names at 256 runes nor redact them, and do not cap the joined summary at MaxContentLen+4096 (Go #2246)"]
async fn test_doc_parity_response_summary_and_function_name_bounds() {
    let at_call_cap = "c".repeat(256);
    let at_resp_cap = "r".repeat(256);
    let over_cap_call = "c".repeat(257);
    let over_cap_resp = "r".repeat(257);
    let many_parts = response_text_parts(40, 100, "a");
    let forty_small = joined_response_pieces(&response_text_piece(&"a".repeat(100)), 40);
    let prefix_runes = |s: &str, n: usize| s.chars().take(n).collect::<String>();
    let names_and_parts = || {
        let mut parts = vec![named_call(&over_cap_call), named_response(&over_cap_resp)];
        parts.extend(many_parts.clone());
        parts
    };

    // finite MaxContentLen caps function names at 256 runes and joined summaries at
    // MaxContentLen plus 4096
    {
        const MAX_LEN: usize = 100;
        let tp = new_test_plugin_with_config(|c| {
            c.max_content_len = MAX_LEN as i64;
            c.custom_metadata_allowlist = vec!["adk_compaction".into()];
        })
        .await;
        let m = mc(&tp);
        after_model(
            &tp,
            &m,
            LlmResponse {
                content: Some(model_content(vec![
                    named_call(&over_cap_call),
                    named_response(&over_cap_resp),
                ])),
                ..LlmResponse::default()
            },
        )
        .await;
        after_model(
            &tp,
            &m,
            LlmResponse {
                content: Some(model_content(many_parts.clone())),
                ..LlmResponse::default()
            },
        )
        .await;
        let mut agent_resp = event("ev-bounded-agent-resp", "test-agent");
        agent_resp.response.content = Some(model_content(many_parts.clone()));
        on_event(&tp, &m, agent_resp).await;
        let mut comp = event("ev-bounded-compaction", "test-agent");
        comp.response.custom_metadata = compaction_metadata(&model_content(many_parts.clone()));
        on_event(&tp, &m, comp).await;
        after_run(&tp, &m).await;

        let rows = completed_rows(&tp).await;
        let llm_rows = rows_of_type(&rows, EVENT_LLM_RESPONSE);
        assert_eq!(llm_rows.len(), 2, "LLM_RESPONSE rows");

        let want_names = format!(
            "call: {at_call_cap}{TRUNCATION_SUFFIX} | resp: {at_resp_cap}{TRUNCATION_SUFFIX}"
        );
        assert_eq!(
            json_str(&llm_rows[0], COLUMN_CONTENT, "response"),
            want_names,
            "LLM_RESPONSE[0] content.response"
        );
        assert_eq!(
            is_truncated(&llm_rows[0]),
            Some(true),
            "LLM_RESPONSE[0] is_truncated"
        );

        let want_joined = format!(
            "{}{TRUNCATION_SUFFIX}",
            prefix_runes(&forty_small, MAX_LEN + 4096)
        );
        let got_joined = json_str(&llm_rows[1], COLUMN_CONTENT, "response");
        assert!(
            got_joined == want_joined,
            "LLM_RESPONSE[1] content.response = {} runes, want {} runes",
            got_joined.chars().count(),
            MAX_LEN + 4096 + TRUNCATION_SUFFIX.len()
        );
        assert_eq!(
            is_truncated(&llm_rows[1]),
            Some(true),
            "LLM_RESPONSE[1] is_truncated"
        );

        let agent_row = only_row_of_type(&rows, EVENT_AGENT_RESPONSE);
        let want_agent = format!("{}{TRUNCATION_SUFFIX}", prefix_runes(&forty_small, MAX_LEN));
        assert_eq!(
            json_str(&agent_row, COLUMN_CONTENT, "response"),
            want_agent,
            "AGENT_RESPONSE content.response"
        );
        assert_eq!(
            is_truncated(&agent_row),
            Some(true),
            "AGENT_RESPONSE is_truncated"
        );

        let comp_row = only_row_of_type(&rows, EVENT_EVENT_COMPACTION);
        assert_eq!(
            json_str(&comp_row, COLUMN_CONTENT, "compacted_content"),
            want_agent,
            "EVENT_COMPACTION content.compacted_content"
        );
        assert_eq!(
            is_truncated(&comp_row),
            Some(true),
            "EVENT_COMPACTION is_truncated"
        );

        // When MaxContentLen exceeds a capped function-name summary, EVENT_COMPACTION retains
        // the 256-rune function-name cap from formatResponseContent.
        let tp_comp = new_test_plugin_with_config(|c| {
            c.max_content_len = 600;
            c.custom_metadata_allowlist = vec!["adk_compaction".into()];
        })
        .await;
        let m_comp = mc(&tp_comp);
        let mut comp = event("ev-compaction-long-fn", "test-agent");
        comp.response.custom_metadata = compaction_metadata(&model_content(vec![
            named_call(&over_cap_call),
            named_response(&over_cap_resp),
        ]));
        on_event(&tp_comp, &m_comp, comp).await;
        after_run(&tp_comp, &m_comp).await;
        let comp_name_row =
            only_row_of_type(&completed_rows(&tp_comp).await, EVENT_EVENT_COMPACTION);
        assert_eq!(
            json_str(&comp_name_row, COLUMN_CONTENT, "compacted_content"),
            want_names,
            "EVENT_COMPACTION long-name content.compacted_content"
        );
        assert_eq!(
            is_truncated(&comp_name_row),
            Some(true),
            "EVENT_COMPACTION long-name is_truncated"
        );
    }

    // CaptureFullMessageContent preserves long function names and multi-part summaries while
    // redacting credentials; then MaxContentLen -1 without CaptureFullMessageContent lifts both
    // length caps while preserving redaction when enabled.
    for (name, max_len, capture_full, secret) in [
        (
            "CaptureFullMessageContent",
            1,
            true,
            "SYNTHETIC_DOC_SECRET_0123456789",
        ),
        (
            "MaxContentLen -1",
            -1,
            false,
            "SYNTHETIC_DOC_NEG_SECRET_0123456789",
        ),
    ] {
        let tp = new_test_plugin_with_config(|c| {
            c.max_content_len = max_len;
            c.capture_full_message_content = capture_full;
            c.custom_metadata_allowlist = vec!["adk_compaction".into()];
        })
        .await;
        let m = mc(&tp);
        after_model(
            &tp,
            &m,
            LlmResponse {
                content: Some(model_content(names_and_parts())),
                ..LlmResponse::default()
            },
        )
        .await;
        let mut agent_resp = event("ev-full-doc-agent-resp", "test-agent");
        agent_resp.response.content = Some(model_content(many_parts.clone()));
        on_event(&tp, &m, agent_resp).await;
        let mut comp = event("ev-full-doc-compaction", "test-agent");
        comp.response.custom_metadata = compaction_metadata(&model_content(names_and_parts()));
        on_event(&tp, &m, comp).await;
        after_model(
            &tp,
            &m,
            LlmResponse {
                content: Some(model_content(vec![named_call(&format!(
                    "{over_cap_call} api_key={secret}"
                ))])),
                ..LlmResponse::default()
            },
        )
        .await;
        after_run(&tp, &m).await;

        let rows = completed_rows(&tp).await;
        let llm_rows = rows_of_type(&rows, EVENT_LLM_RESPONSE);
        assert_eq!(llm_rows.len(), 2, "{name}: LLM_RESPONSE rows");

        let want_full = format!("call: {over_cap_call} | resp: {over_cap_resp} | {forty_small}");
        let got = json_str(&llm_rows[0], COLUMN_CONTENT, "response");
        assert!(
            got == want_full,
            "{name}: LLM_RESPONSE[0] content.response = {} runes, want {} runes",
            got.chars().count(),
            want_full.chars().count()
        );
        assert_eq!(
            is_truncated(&llm_rows[0]),
            Some(false),
            "{name}: LLM_RESPONSE[0] is_truncated"
        );

        let agent_row = only_row_of_type(&rows, EVENT_AGENT_RESPONSE);
        let got = json_str(&agent_row, COLUMN_CONTENT, "response");
        assert!(
            got == forty_small,
            "{name}: AGENT_RESPONSE content.response = {} runes, want {} runes",
            got.chars().count(),
            forty_small.chars().count()
        );
        assert_eq!(
            is_truncated(&agent_row),
            Some(false),
            "{name}: AGENT_RESPONSE is_truncated"
        );

        let comp_row = only_row_of_type(&rows, EVENT_EVENT_COMPACTION);
        let got = json_str(&comp_row, COLUMN_CONTENT, "compacted_content");
        assert!(
            got == want_full,
            "{name}: EVENT_COMPACTION content.compacted_content = {} runes, want {} runes",
            got.chars().count(),
            want_full.chars().count()
        );
        assert_eq!(
            is_truncated(&comp_row),
            Some(false),
            "{name}: EVENT_COMPACTION is_truncated"
        );

        let got_redacted = json_str(&llm_rows[1], COLUMN_CONTENT, "response");
        let want_redacted = format!("call: {over_cap_call} api_key={REDACTED_VALUE}");
        assert_eq!(
            got_redacted, want_redacted,
            "{name}: LLM_RESPONSE[1] content.response"
        );
        assert!(
            !got_redacted.contains(secret) && !got_redacted.contains(TRUNCATION_SUFFIX),
            "{name}: LLM_RESPONSE[1] content.response = {got_redacted:?}, want secret redacted without length truncation"
        );
        assert_eq!(
            is_truncated(&llm_rows[1]),
            Some(true),
            "{name}: LLM_RESPONSE[1] is_truncated when redaction scrubs a credential"
        );
    }

    // MaxContentLen -1 with DisableSensitiveKeyRedaction keeps the raw text.
    let secret = "SYNTHETIC_DOC_NEG_SECRET_0123456789";
    let tp = new_test_plugin_with_config(|c| {
        c.max_content_len = -1;
        c.disable_sensitive_key_redaction = true;
    })
    .await;
    let m = mc(&tp);
    after_model(
        &tp,
        &m,
        LlmResponse {
            content: Some(model_content(vec![named_call(&format!(
                "{over_cap_call} api_key={secret}"
            ))])),
            ..LlmResponse::default()
        },
    )
    .await;
    after_run(&tp, &m).await;
    let unredacted_row = only_row_of_type(&completed_rows(&tp).await, EVENT_LLM_RESPONSE);
    let want_raw = format!("call: {over_cap_call} api_key={secret}");
    assert_eq!(
        json_str(&unredacted_row, COLUMN_CONTENT, "response"),
        want_raw,
        "unredacted LLM_RESPONSE content.response"
    );
    assert_eq!(
        is_truncated(&unredacted_row),
        Some(false),
        "unredacted LLM_RESPONSE is_truncated"
    );
}
