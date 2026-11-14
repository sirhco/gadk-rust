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

//! Part of the port of ADK-Go `plugin/agentanalytics/bigquery_agent_analytics_plugin_test.go`:
//! the 18 Go tests from `TestToolConfirmationRequired_CompletesCall` to
//! `TestCredentialShapesNeverReachARow` in the Go file's order (the exact list is in the 7b-3a
//! plan's contracts, Appendix A; the rest of the file is in `part_3`).
//!
//! Go → gadk: Go's `mockTransport` BigQuery clients are a `ScriptedClient` behind the client's
//! endpoint; Go's gRPC write server and connection are a `FakeWriteServer` and its
//! `WriteClient`, whose shared `closed` flag is Go's `conn.GetState() == connectivity.Shutdown`.
//! A Go `t.Errorf` inside a transport becomes a recorded unexpected request, asserted after.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use gadk_agent::{Tool, ToolError};
use gadk_genai::{
    CodeExecutionResult, Content, ExecutableCode, FunctionCall, FunctionDeclaration,
    FunctionResponse, GenerateContentConfig, Language, Outcome, Part, Schema, Type,
};
use gadk_model::rest::test_util::{FakeResponse, RecordedRequest, ScriptedClient};
use gadk_model::{LlmRequest, LlmResponse, ModelError};
use gadk_session::Event;
use pretty_assertions::assert_eq;
use serde_json::{Map, Value, json};

use super::helpers::{CallbackErrors, FakeTool, check_json_at, fake_tool, parts_of, text_content};
use super::support::{
    MockCtx, TestPlugin, check_json, event_types, json_object_column, lookup,
    new_test_plugin_with_config, only_row_of_type, rows_of_type, setup_test_plugin,
    until_event_type, until_invocation_end,
};
use crate::WriteClient;
use crate::bigquery::BigQueryClient;
use crate::bigquery::fake::RecordingLogger;
use crate::config::{ClientOptions, Config, Logger, SCHEMA_VERSION};
use crate::context::Context;
use crate::error::Error;
use crate::event_types::*;
use crate::parity_tests::plugin_agentanalytics_batch_processor::support::arrow_writer_schema_columns;
use crate::plugin::new_bigquery_agent_analytics_plugin_with_clients;
use crate::plugin::rows::{derive_scope, identity_of, llm_request_attributes, parse_node_path};
use crate::plugin::{SCOPE_KIND_FUNCTION_CALL, SCOPE_KIND_NODE_RUN};
use crate::redaction::{
    COLUMN_CONTENT, COLUMN_CONTENT_PARTS, COLUMN_LATENCY_MS, REDACTED_VALUE,
    UNPARSEABLE_JSON_PLACEHOLDER,
};
use crate::row::{Row, RowValue};
use crate::schema::{EVENT_ID_FIELD, Schema as BqSchema, TableSchema, events_schema};
use crate::testutil::fake_write_server::FakeWriteServer;
use crate::trace::{CallbackCtx, Identity};

// ---------------------------------------------------------------------------------------------
// File-local Go test helpers
// ---------------------------------------------------------------------------------------------

/// Go `fmt.Errorf("error tool %q %w", name, tool.ErrConfirmationRequired)`: the flow's wrapped
/// confirmation error (gadk-engine's private `NamedToolError`).
#[derive(Debug)]
struct ConfirmationOutcome {
    name: String,
    source: ToolError,
}

impl std::fmt::Display for ConfirmationOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "error tool \"{}\" {}", self.name, self.source)
    }
}

impl std::error::Error for ConfirmationOutcome {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// Go `errors.New(text)` handed to a tool callback.
fn tool_error(text: &str) -> Arc<ToolError> {
    Arc::new(ToolError::Execution(text.into()))
}

/// Go `errors.New(text)` handed to a model callback.
fn model_error(text: &str) -> Arc<ModelError> {
    Arc::new(ModelError::Other(text.into()))
}

/// Go `checkJSON(t, name, got, want)`.
fn check(name: &str, got: &Value, want: &str) {
    if let Err(diff) = check_json(name, got, want) {
        panic!("{diff}");
    }
}

/// A string column of `row` (Go `row[name].(string)`), "" when missing.
fn str_of(row: &Row, name: &str) -> String {
    row.get(name)
        .and_then(RowValue::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// A Go `nil` args map.
fn no_args() -> Map<String, Value> {
    Map::new()
}

/// `json!({..})` as an args / result map.
fn obj(v: Value) -> Map<String, Value> {
    v.as_object().cloned().unwrap_or_default()
}

/// Go `collectRows(t, requests, done)` over the requests received since the `from`-th: Go's
/// channel hands each request to one reader only, so a later collection sees only new rows.
async fn collect_rows_from(
    tp: &TestPlugin,
    from: usize,
    done: impl Fn(&[Row]) -> bool,
) -> Vec<Row> {
    let rows_since = || -> Vec<Row> {
        tp.server
            .append_requests()
            .iter()
            .skip(from)
            .flat_map(|r| super::support::decode_arrow_rows(&r.request))
            .collect()
    };
    let wait = async {
        loop {
            let n = tp.server.append_requests().len();
            let rows = rows_since();
            if done(&rows) {
                return rows;
            }
            tp.server.wait_for_append_requests(n + 1).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(5), wait)
        .await
        .unwrap_or_else(|_| {
            panic!(
                "timed out collecting rows; got {:?}",
                event_types(&rows_since())
            )
        })
}

/// Go `bq.NewClient(ctx, "test-project", option.WithHTTPClient(&http.Client{Transport:
/// transport}))`: a BigQuery client whose requests go to `transport`.
fn bq_client(transport: Arc<ScriptedClient>) -> Arc<BigQueryClient> {
    let options = ClientOptions {
        http_client: Some(transport),
        without_authentication: true,
        endpoint: Some("http://bq.test/bigquery/v2/".into()),
        ..ClientOptions::default()
    };
    Arc::new(BigQueryClient::new("test-project", &options).expect("bigquery client"))
}

/// Go `newTestWriteClient(t)`: a write client of a fake write server; its `closed` flag is
/// Go's connection state.
fn new_test_write_client() -> (FakeWriteServer, WriteClient) {
    let server = FakeWriteServer::new();
    let client = server.client().expect("write client");
    (server, client)
}

/// Go `conn.GetState() == connectivity.Shutdown`.
fn is_shut_down(client: &WriteClient) -> bool {
    client.closed.load(Ordering::SeqCst)
}

/// Go `DefaultConfig()` for `test-project.test_dataset.test-table`.
fn table_config() -> Config {
    Config {
        project_id: "test-project".into(),
        dataset_id: "test_dataset".into(),
        table_name: "test-table".into(),
        ..Config::default()
    }
}

/// Go `p.Close()` of a constructed plugin.
async fn close_plugin(p: &gadk_agent::Plugin) {
    let _ = (p.close().expect("close function"))().await;
}

/// Go `errors.As(err, &apiErr) && apiErr.Code == code`.
fn has_api_code(err: &Error, code: u16) -> bool {
    matches!(err.find(&mut |e| matches!(e, Error::Api(_))), Some(Error::Api(a)) if a.code == code)
}

/// Go's `{"error":{"code":N,"message":...}}` response.
fn api_error(status: u16, message: &str) -> FakeResponse {
    FakeResponse::json_status(
        status,
        format!(r#"{{"error":{{"code":{status},"message":"{message}"}}}}"#),
    )
}

/// Go's job response of `mockTransport`.
fn done_job() -> FakeResponse {
    FakeResponse::json(
        r#"{"jobReference":{"projectId":"test-project","jobId":"test-job"},"status":{"state":"DONE"}}"#,
    )
}

/// The names of the `schema.fields` of a table request body.
fn body_field_names(body: &Value) -> Vec<String> {
    body.pointer("/schema/fields")
        .and_then(Value::as_array)
        .map(|fs| {
            fs.iter()
                .filter_map(|f| f.get("name").and_then(Value::as_str).map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// The `labels` of a table request body.
fn body_labels(body: &Value) -> BTreeMap<String, String> {
    body.get("labels")
        .and_then(Value::as_object)
        .map(|m| {
            m.iter()
                .map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_owned()))
                .collect()
        })
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------------------------

// parity: adapted: Go's `fmt.Errorf("error tool %q %w", ...)` is a local error with the confirmation sentinel as its source (gadk-engine's NamedToolError is private), handed over as ToolError::Execution, as the flow does
#[tokio::test]
async fn test_tool_confirmation_required_completes_call() {
    let tp =
        new_test_plugin_with_config(|c| c.final_response_tool_names = vec!["deploy".into()]).await;
    let p = &tp.plugin;
    let m_ctx = tp.ctx.with_function_call("call-1");
    let ctx = m_ctx.context();
    let deploy_tool = fake_tool("deploy");
    let args = obj(json!({"region": "us-central1"}));
    let tool_err = Arc::new(ToolError::Execution(Box::new(ConfirmationOutcome {
        name: "deploy".into(),
        source: ToolError::ConfirmationRequired,
    })));
    let mut errs = CallbackErrors::default();
    errs.add(
        (p.before_tool_callback().unwrap())(ctx.clone(), deploy_tool.clone(), args.clone()).await,
    );
    let res = (p.on_tool_error_callback().unwrap())(
        ctx.clone(),
        deploy_tool.clone(),
        args.clone(),
        tool_err.clone(),
    )
    .await;
    assert!(
        matches!(res, Ok(None)),
        "OnToolErrorCallback() = {res:?}; want nil, nil"
    );
    errs.add(
        (p.after_tool_callback().unwrap())(
            ctx.clone(),
            deploy_tool.clone(),
            args.clone(),
            Map::new(),
            Some(tool_err),
        )
        .await,
    );
    // The user confirms the call, and ADK runs the tool.
    errs.add(
        (p.before_tool_callback().unwrap())(ctx.clone(), deploy_tool.clone(), args.clone()).await,
    );
    errs.add(
        (p.after_tool_callback().unwrap())(
            ctx.clone(),
            deploy_tool,
            args,
            obj(json!({"cluster_id": "c-123"})),
            None,
        )
        .await,
    );
    errs.check();
    (p.after_run_callback().unwrap())(m_ctx.invocation()).await;

    let rows = tp
        .collect_rows(until_event_type(EVENT_INVOCATION_COMPLETED))
        .await;
    let want_types = vec![
        EVENT_TOOL_STARTING,
        EVENT_TOOL_COMPLETED, // waits for confirmation
        EVENT_TOOL_STARTING,
        EVENT_TOOL_COMPLETED,
        EVENT_AGENT_RESPONSE, // confirmed
        EVENT_INVOCATION_COMPLETED,
    ];
    assert_eq!(event_types(&rows), want_types, "event types mismatch");
    let row = rows_of_type(&rows, EVENT_TOOL_COMPLETED)[0].clone();
    assert_eq!(str_of(&row, "status"), "OK", "status");
    assert!(
        !row.contains_key("error_message"),
        "error_message = {:?}, want none",
        row.get("error_message")
    );
    check(
        "content",
        &json_object_column(&row, "content"),
        r#"{"tool":"deploy","result":{"error":"error tool \"deploy\" requires confirmation, please approve or reject"},"tool_origin":"UNKNOWN"}"#,
    );
    check_json_at(
        &json_object_column(&row, "attributes"),
        r#""call-1""#,
        &["adk", "function_call_id"],
    );
    assert_eq!(
        row.get("span_id"),
        rows_of_type(&rows, EVENT_TOOL_STARTING)[0].get("span_id"),
        "TOOL_COMPLETED span_id, want the TOOL_STARTING span_id"
    );
    assert!(
        row.contains_key(COLUMN_LATENCY_MS),
        "TOOL_COMPLETED row has no latency_ms"
    );
    check(
        "AGENT_RESPONSE content",
        &json_object_column(&only_row_of_type(&rows, EVENT_AGENT_RESPONSE), "content"),
        r#"{"response":{"region":"us-central1"}}"#,
    );
}

// parity: adapted: gadk's tool callbacks always receive a tool (Arc<dyn Tool>); Go's nil tool is a tool whose name is "" (toolName's result for nil), with Go's other arguments
#[tokio::test]
async fn test_tool_callbacks_nil_tool_does_not_panic() {
    let tp = setup_test_plugin().await;
    let p = &tp.plugin;
    let m_ctx = &tp.ctx;
    let ctx = m_ctx.context();
    let nil_tool = fake_tool("");
    let mut errs = CallbackErrors::default();
    errs.add(
        (p.before_tool_callback().unwrap())(ctx.clone(), nil_tool.clone(), obj(json!({"q": "1"})))
            .await,
    );
    errs.add(
        (p.on_tool_error_callback().unwrap())(
            ctx.clone(),
            nil_tool.clone(),
            obj(json!({"q": "1"})),
            tool_error("tool not found"),
        )
        .await,
    );
    errs.add(
        (p.after_tool_callback().unwrap())(
            ctx.clone(),
            nil_tool,
            obj(json!({"q": "1"})),
            obj(json!({"ok": true})),
            None,
        )
        .await,
    );
    errs.check();
    (p.after_run_callback().unwrap())(m_ctx.invocation()).await;

    let rows = tp
        .collect_rows(until_event_type(EVENT_INVOCATION_COMPLETED))
        .await;
    assert_eq!(
        event_types(&rows),
        vec![
            EVENT_TOOL_STARTING,
            EVENT_TOOL_ERROR,
            EVENT_TOOL_COMPLETED,
            EVENT_INVOCATION_COMPLETED
        ],
        "event types mismatch"
    );
    let start_row = only_row_of_type(&rows, EVENT_TOOL_STARTING);
    let err_row = only_row_of_type(&rows, EVENT_TOOL_ERROR);
    let done_row = only_row_of_type(&rows, EVENT_TOOL_COMPLETED);
    assert!(
        err_row.get("span_id") == start_row.get("span_id")
            && done_row.get("span_id") == start_row.get("span_id"),
        "span_ids = ({:?}, {:?}, {:?}), want all equal to TOOL_STARTING span_id",
        start_row.get("span_id"),
        err_row.get("span_id"),
        done_row.get("span_id")
    );
    check(
        "TOOL_STARTING content",
        &json_object_column(&start_row, "content"),
        r#"{"tool":"","args":{"q":"1"},"tool_origin":"UNKNOWN"}"#,
    );
    check(
        "TOOL_ERROR content",
        &json_object_column(&err_row, "content"),
        r#"{"tool":"","args":{"q":"1"},"tool_origin":"UNKNOWN"}"#,
    );
    check(
        "TOOL_COMPLETED content",
        &json_object_column(&done_row, "content"),
        r#"{"tool":"","result":{"ok":true},"tool_origin":"UNKNOWN"}"#,
    );
}

#[tokio::test]
async fn test_after_model_callback_status() {
    let tp = setup_test_plugin().await;
    let p = &tp.plugin;
    let m_ctx = &tp.ctx;
    let ctx = m_ctx.context();
    let res = |version: &str| LlmResponse {
        model_version: version.into(),
        ..LlmResponse::default()
    };
    let responses: Vec<(LlmResponse, Option<Arc<ModelError>>)> = vec![
        (res("ok"), None),
        (
            LlmResponse {
                content: Some(Content {
                    parts: vec![Part {
                        function_call: Some(FunctionCall {
                            name: "lookup".into(),
                            ..FunctionCall::default()
                        }),
                        ..Part::default()
                    }],
                    role: String::new(),
                }),
                ..res("function-call")
            },
            None,
        ),
        (res("callback-error"), Some(model_error("stream broken"))),
        (
            LlmResponse {
                error_code: "SAFETY".into(),
                error_message: "blocked by safety filters".into(),
                ..res("error-message")
            },
            None,
        ),
        (
            LlmResponse {
                error_code: "MAX_TOKENS".into(),
                ..res("error-code")
            },
            None,
        ),
        (
            LlmResponse {
                error_message: "quota exceeded".into(),
                ..res("message-only")
            },
            None,
        ),
        (
            LlmResponse {
                partial: true,
                ..res("partial")
            },
            None,
        ),
    ];
    let mut errs = CallbackErrors::default();
    for (r, err) in responses {
        errs.add(
            (p.after_model_callback().unwrap())(ctx.clone(), r, err)
                .await
                .1,
        );
    }
    errs.check();
    (p.after_run_callback().unwrap())(m_ctx.invocation()).await;

    let s = |v: &str| RowValue::from(v);
    let want: BTreeMap<String, BTreeMap<&str, RowValue>> = [
        ("ok", vec![("status", s("OK"))]),
        (
            "function-call",
            vec![
                ("status", s("OK")),
                ("content", s(r#"{"response":"call: lookup"}"#)),
            ],
        ),
        (
            "callback-error",
            vec![
                ("status", s("ERROR")),
                ("error_message", s("stream broken")),
            ],
        ),
        (
            "error-message",
            vec![
                ("status", s("OK")),
                ("error_message", s("blocked by safety filters")),
            ],
        ),
        ("error-code", vec![("status", s("OK"))]),
        (
            "message-only",
            vec![("status", s("OK")), ("error_message", s("quota exceeded"))],
        ),
    ]
    .into_iter()
    .map(|(k, cols)| (k.to_owned(), cols.into_iter().collect()))
    .collect();
    let rows = tp
        .collect_rows(until_event_type(EVENT_INVOCATION_ERROR))
        .await;
    let inv_err = only_row_of_type(&rows, EVENT_INVOCATION_ERROR);
    assert_eq!(
        str_of(&inv_err, "error_message"),
        "stream broken",
        "INVOCATION_ERROR error_message"
    );
    let mut got: BTreeMap<String, BTreeMap<&str, RowValue>> = BTreeMap::new();
    for row in rows_of_type(&rows, EVENT_LLM_RESPONSE) {
        let attrs = json_object_column(&row, "attributes");
        let version = match lookup(&attrs, &["model_version"]) {
            Some(Value::String(v)) => v.clone(),
            Some(v) => v.to_string(),
            None => "<nil>".to_owned(),
        };
        let mut columns = BTreeMap::new();
        columns.insert(
            "status",
            row.get("status").cloned().unwrap_or(RowValue::Null),
        );
        if let Some(msg) = row.get("error_message") {
            columns.insert("error_message", msg.clone());
        }
        if let Some(c) = row.get("content") {
            columns.insert("content", c.clone());
        }
        got.insert(version, columns);
    }
    assert_eq!(want, got, "LLM_RESPONSE rows mismatch");
}

#[tokio::test]
async fn test_truncation() {
    struct Case {
        name: &'static str,
        mutate: fn(&mut Config),
        want_summary: &'static str,
        want_err_msg: &'static str,
        want_trunc: bool,
    }
    let tests = [
        Case {
            name: "MaxContentLen truncates",
            mutate: |c| c.max_content_len = 5,
            want_summary: "hello...[TRUNCATED]",
            want_err_msg: "model...[TRUNCATED]",
            want_trunc: true,
        },
        Case {
            name: "CaptureFullMessageContent keeps the full text",
            mutate: |c| {
                c.max_content_len = 5;
                c.capture_full_message_content = true;
            },
            want_summary: "hello world",
            want_err_msg: "model unavailable",
            want_trunc: false,
        },
    ];
    for tt in tests {
        let tp = new_test_plugin_with_config(tt.mutate).await;
        let p = &tp.plugin;
        let m_ctx = &tp.ctx;
        let ctx = m_ctx.context();
        let mut errs = CallbackErrors::default();
        errs.add(
            (p.on_user_message_callback().unwrap())(
                m_ctx.invocation(),
                text_content("user", "hello world"),
            )
            .await,
        );
        errs.add(
            (p.on_model_error_callback().unwrap())(
                ctx.clone(),
                LlmRequest::default(),
                model_error("model unavailable"),
            )
            .await,
        );
        errs.check();
        (p.after_run_callback().unwrap())(m_ctx.invocation()).await;

        let rows = tp.collect_rows(until_invocation_end).await;
        let row = only_row_of_type(&rows, EVENT_USER_MESSAGE_RECEIVED);
        check_json_at(
            &json_object_column(&row, "content"),
            &Value::from(tt.want_summary).to_string(),
            &["text_summary"],
        );
        assert_eq!(
            row.get("is_truncated"),
            Some(&RowValue::Bool(tt.want_trunc)),
            "{}: is_truncated",
            tt.name
        );
        assert_eq!(
            str_of(&only_row_of_type(&rows, EVENT_LLM_ERROR), "error_message"),
            tt.want_err_msg,
            "{}: LLM_ERROR error_message",
            tt.name
        );
    }
}

// parity: adapted: Go's panic value is a string; a Rust panic message is a &str / String payload, which recover_callback names as Go's `string`
#[tokio::test]
async fn test_log_event_recovers_from_panics() {
    let logger = Arc::new(RecordingLogger::default());
    let log: Arc<dyn Logger> = logger.clone();
    let tp = new_test_plugin_with_config(move |c| c.logger = Some(log)).await;
    let p = &tp.plugin;
    let m_ctx = &tp.ctx;
    let ctx = m_ctx.context();
    let broken: Arc<dyn Tool> = Arc::new(FakeTool {
        name: "broken".into(),
        panic_on_description: true,
        ..FakeTool::default()
    });
    let mut errs = CallbackErrors::default();
    errs.add((p.before_tool_callback().unwrap())(ctx.clone(), broken.clone(), no_args()).await);
    errs.add(
        (p.after_tool_callback().unwrap())(
            ctx.clone(),
            broken,
            no_args(),
            obj(json!({"ok": true})),
            None,
        )
        .await,
    );
    errs.check();
    (p.after_run_callback().unwrap())(m_ctx.invocation()).await;

    let rows = tp
        .collect_rows(until_event_type(EVENT_INVOCATION_COMPLETED))
        .await;
    assert_eq!(
        event_types(&rows),
        vec![EVENT_TOOL_COMPLETED, EVENT_INVOCATION_COMPLETED],
        "event types mismatch"
    );
    let want = "dropped TOOL_STARTING event: recovered panic of type string";
    assert!(
        logger.contains(want),
        "log does not contain {want:?}:\n{logger}"
    );
    assert!(
        !logger.contains("tool description unavailable"),
        "log contains the panic value:\n{logger}"
    );
}

// parity: adapted: Go's noAgentContext (an invocation whose agent accessor panics, read as "" by identityOf's guard) is an ADK context whose agent is named "" (gadk contexts always hold an agent, so identity_of has no guard)
#[tokio::test]
async fn test_identity_agent_falls_back_to_event_author() {
    let tp = setup_test_plugin().await;
    let p = &tp.plugin;
    let ctx = MockCtx {
        agent_name: String::new(),
        ..tp.ctx.clone()
    };
    let mut ev = Event {
        author: "event-author".into(),
        ..Event::default()
    };
    ev.response.content = Some(text_content("model", "hi"));
    if let Err(err) = (p.on_event_callback().unwrap())(ctx.invocation(), ev).await {
        panic!("OnEventCallback error: {err}");
    }
    (p.after_run_callback().unwrap())(ctx.invocation()).await;

    let rows = tp
        .collect_rows(until_event_type(EVENT_INVOCATION_COMPLETED))
        .await;
    let row = only_row_of_type(&rows, EVENT_AGENT_RESPONSE);
    assert_eq!(
        str_of(&row, "agent"),
        "event-author",
        "agent, want the event author"
    );
    assert_eq!(str_of(&row, "session_id"), "sess-id", "session_id");
    let completed = only_row_of_type(&rows, EVENT_INVOCATION_COMPLETED);
    assert!(
        !completed.contains_key("agent"),
        "INVOCATION_COMPLETED agent = {:?}, want none",
        completed.get("agent")
    );
}

// parity: adapted: Go's context.Background() is CallbackCtx::background(); Identity has no `==` (its state is a trait object), so the fields are compared
#[test]
fn test_identity_of_plain_context() {
    let got = identity_of(&CallbackCtx::background());
    let zero = Identity::default();
    assert!(
        got.agent_name == zero.agent_name
            && got.session_id == zero.session_id
            && got.invocation_id == zero.invocation_id
            && got.user_id == zero.user_id
            && got.app_name == zero.app_name
            && got.branch == zero.branch
            && got.path == zero.path
            && got.state.is_none(),
        "identityOf(context.Background()) = {:?}, want the zero identity",
        (
            &got.agent_name,
            &got.session_id,
            &got.invocation_id,
            &got.user_id,
            &got.app_name,
            &got.branch,
            &got.path,
            got.state.is_some()
        )
    );
}

// parity: adapted: LlmRequest.tools holds each tool as an `Arc<dyn Tool>` behind `dyn Any` (the gadk flow's form); Go's declaredTool is a FakeTool with a declaration
#[test]
fn test_llm_request_attributes_tools() {
    fn packed(t: FakeTool) -> Arc<dyn std::any::Any + Send + Sync> {
        let t: Arc<dyn Tool> = Arc::new(t);
        Arc::new(t)
    }
    let mut req = LlmRequest {
        config: Some(GenerateContentConfig {
            tools: vec![gadk_genai::Tool {
                function_declarations: vec![FunctionDeclaration {
                    name: "from_config".into(),
                    description: "Declared in the config".into(),
                    parameters: Some(Schema {
                        r#type: Some(Type::Object),
                        ..Schema::default()
                    }),
                    ..FunctionDeclaration::default()
                }],
                ..gadk_genai::Tool::default()
            }],
            ..GenerateContentConfig::default()
        }),
        ..LlmRequest::default()
    };
    req.tools.insert(
        "lookup".into(),
        packed(FakeTool {
            name: "lookup".into(),
            description: "Looks things up".into(),
            decl: Some(FunctionDeclaration {
                name: "lookup".into(),
                description: "Declaration description".into(),
                parameters: Some(Schema {
                    r#type: Some(Type::String),
                    ..Schema::default()
                }),
                parameters_json_schema: Some(json!({"type": "object"})),
                ..FunctionDeclaration::default()
            }),
            ..FakeTool::default()
        }),
    );
    req.tools.insert(
        "from_config".into(),
        packed(FakeTool {
            name: "from_config".into(),
            ..FakeTool::default()
        }),
    );
    let attrs = llm_request_attributes(&req);
    let tools = attrs
        .get("tools")
        .unwrap_or_else(|| panic!("llmRequestAttributes() has no tools"));
    let (b, _) = crate::formatter::smart_truncate(tools, -1).expect("SmartTruncate()");
    let got: Value = serde_json::from_slice(&b)
        .unwrap_or_else(|e| panic!("json.Unmarshal({}) error: {e}", String::from_utf8_lossy(&b)));
    check(
        "tools",
        &got,
        concat!(
            r#"[{"name":"from_config","description":"Declared in the config","parameters":{"type":"OBJECT"}},"#,
            r#"{"name":"lookup","description":"Looks things up","parameters":{"type":"object"}}]"#
        ),
    );
}

#[test]
fn test_parse_node_path() {
    // (path, want run ID, want parent run ID; None = Go's nil)
    let tests: [(&str, &str, Option<&str>); 7] = [
        ("", "", None),
        ("agent", "", None),
        ("agent@1", "1", None),
        ("wf@0/agent@3", "3", Some("0")),
        ("wf/agent@3", "3", None),
        ("wf@0/agent", "", Some("0")),
        ("root/wf@0/agent@3", "3", Some("0")),
    ];
    for (path, want_run_id, want_parent) in tests {
        let (run_id, parent_run_id) = parse_node_path(path);
        let want_parent_v = want_parent.map_or(Value::Null, Value::from);
        assert!(
            run_id == want_run_id && parent_run_id == want_parent_v,
            "parseNodePath({path:?}) = ({run_id:?}, {parent_run_id}), want ({want_run_id:?}, {want_parent_v})"
        );
    }
}

#[test]
fn test_derive_scope() {
    let tests = [
        ("wf@0/review@2", SCOPE_KIND_NODE_RUN),
        ("review@2", SCOPE_KIND_NODE_RUN),
        ("adk-call-123", SCOPE_KIND_FUNCTION_CALL),
        ("wf@0/adk-call-123", SCOPE_KIND_FUNCTION_CALL),
    ];
    for (scope, want_kind) in tests {
        let want = obj(json!({"id": scope, "kind": want_kind}));
        assert_eq!(want, derive_scope(scope), "deriveScope({scope:?}) mismatch");
    }
}

/// Go's `bqFieldJSON` list of `schema` without the field `omit`, types overridden by
/// `override_type` (the test's `toJSONFields`).
fn to_json_fields(schema: &BqSchema, omit: &str, override_type: &[(&str, &str)]) -> Value {
    let fields: Vec<Value> = schema
        .iter()
        .filter(|f| f.name != omit)
        .map(|f| {
            let typ = override_type
                .iter()
                .find(|(n, _)| *n == f.name)
                .map_or(f.field_type.as_str(), |(_, t)| *t);
            let mode = if f.required {
                "REQUIRED"
            } else if f.repeated {
                "REPEATED"
            } else {
                "NULLABLE"
            };
            let mut m = Map::new();
            m.insert("name".into(), Value::from(f.name.as_str()));
            m.insert("type".into(), Value::from(typ));
            m.insert("mode".into(), Value::from(mode));
            if !f.schema.is_empty() {
                m.insert(
                    "fields".into(),
                    to_json_fields(&f.schema, omit, override_type),
                );
            }
            Value::Object(m)
        })
        .collect();
    if fields.is_empty() {
        // Go's nil slice.
        Value::Null
    } else {
        Value::Array(fields)
    }
}

// parity: adapted: Go's mockTransport is a ScriptedClient (its state in atomics / mutexes); the gRPC write server is a FakeWriteServer
#[tokio::test]
async fn test_new_big_query_agent_analytics_plugin_upgrades_existing_schema() {
    struct Case {
        name: &'static str,
        existing: Value,
        disable_upgrade: bool,
        patch_status: u16,
        retry_upgraded: bool,
        want_patch: bool,
        want_patched: Vec<&'static str>,
        want_err_match: &'static str,
    }
    let schema = events_schema();
    let tests = vec![
        Case {
            name: "legacy 16-column table without event_id is upgraded in place",
            existing: to_json_fields(&schema, "event_id", &[]),
            disable_upgrade: false,
            patch_status: 200,
            retry_upgraded: false,
            want_patch: true,
            want_patched: vec![
                "timestamp",
                "event_type",
                "agent",
                "session_id",
                "invocation_id",
                "user_id",
                "trace_id",
                "span_id",
                "parent_span_id",
                "content",
                "content_parts",
                "attributes",
                "latency_ms",
                "status",
                "error_message",
                "is_truncated",
                "event_id",
            ],
            want_err_match: "",
        },
        Case {
            name: "existing table with empty schema is populated with full schema",
            existing: Value::Null,
            disable_upgrade: false,
            patch_status: 200,
            retry_upgraded: false,
            want_patch: true,
            want_patched: vec![
                "timestamp",
                "event_id",
                "event_type",
                "agent",
                "session_id",
                "invocation_id",
                "user_id",
                "trace_id",
                "span_id",
                "parent_span_id",
                "content",
                "content_parts",
                "attributes",
                "latency_ms",
                "status",
                "error_message",
                "is_truncated",
            ],
            want_err_match: "",
        },
        Case {
            name: "412 precondition failed re-fetches metadata and succeeds when schema is already upgraded",
            existing: to_json_fields(&schema, "event_id", &[]),
            disable_upgrade: false,
            patch_status: 412,
            retry_upgraded: true,
            want_patch: true,
            want_patched: vec![],
            want_err_match: "",
        },
        Case {
            name: "DisableSchemaUpgrade skips PATCH on legacy table",
            existing: to_json_fields(&schema, "event_id", &[]),
            disable_upgrade: true,
            patch_status: 200,
            retry_upgraded: false,
            want_patch: false,
            want_patched: vec![],
            want_err_match: "",
        },
        Case {
            name: "up-to-date 17-column table does not trigger PATCH",
            existing: to_json_fields(&schema, "", &[]),
            disable_upgrade: false,
            patch_status: 200,
            retry_upgraded: false,
            want_patch: false,
            want_patched: vec![],
            want_err_match: "",
        },
        Case {
            name: "incompatible existing column type fails initialization",
            existing: to_json_fields(&schema, "", &[("event_id", "INTEGER")]),
            disable_upgrade: false,
            patch_status: 200,
            retry_upgraded: false,
            want_patch: false,
            want_patched: vec![],
            want_err_match: "incompatible BigQuery schema field",
        },
        Case {
            name: "PATCH failure returns an error",
            existing: to_json_fields(&schema, "event_id", &[]),
            disable_upgrade: false,
            patch_status: 403,
            retry_upgraded: false,
            want_patch: true,
            want_patched: vec![],
            want_err_match: "failed to upgrade BigQuery table",
        },
    ];

    for tt in tests {
        let name = tt.name;
        let mut config = table_config();
        config.disable_schema_upgrade = tt.disable_upgrade;

        let get_body = json!({"etag": "etag-1", "schema": {"fields": tt.existing}}).to_string();
        let upgraded_body =
            json!({"etag": "etag-2", "schema": {"fields": to_json_fields(&schema, "", &[])}})
                .to_string();

        let patch_called = Arc::new(AtomicBool::new(false));
        let patched_columns = Arc::new(parking_lot::Mutex::new(Vec::<String>::new()));
        let patched_labels = Arc::new(parking_lot::Mutex::new(BTreeMap::<String, String>::new()));
        let (called, columns, labels) = (
            Arc::clone(&patch_called),
            Arc::clone(&patched_columns),
            Arc::clone(&patched_labels),
        );
        let (retry_upgraded, patch_status) = (tt.retry_upgraded, tt.patch_status);
        let transport = ScriptedClient::new(move |r: &RecordedRequest| {
            let table = r.path.contains("/datasets/test_dataset/tables/test-table");
            if r.method == "GET" && table {
                if called.load(Ordering::SeqCst) && retry_upgraded {
                    return FakeResponse::json(upgraded_body.clone());
                }
                return FakeResponse::json(get_body.clone());
            }
            if (r.method == "PATCH" || r.method == "PUT") && table {
                called.store(true, Ordering::SeqCst);
                if let Ok(body) = serde_json::from_slice::<Value>(&r.body) {
                    *labels.lock() = body_labels(&body);
                    columns.lock().extend(body_field_names(&body));
                }
                if patch_status != 200 {
                    return api_error(patch_status, "patch status");
                }
                return FakeResponse::json(r.body_text());
            }
            if r.path.contains("/jobs") {
                return done_job();
            }
            FakeResponse::json("{}")
        });

        let (_server, write_client) = new_test_write_client();
        let result = new_bigquery_agent_analytics_plugin_with_clients(
            &Context::background(),
            config,
            Some(bq_client(transport)),
            Some(write_client),
        )
        .await;
        if !tt.want_err_match.is_empty() {
            match &result {
                Err(err) if err.to_string().contains(tt.want_err_match) => {}
                _ => panic!(
                    "{name}: NewBigQueryAgentAnalyticsPluginWithClients error = {:?}, want match {:?}",
                    result.as_ref().err().map(ToString::to_string),
                    tt.want_err_match
                ),
            }
        } else if let Err(err) = &result {
            panic!("{name}: NewBigQueryAgentAnalyticsPluginWithClients unexpected error: {err}");
        }
        let called = patch_called.load(Ordering::SeqCst);
        assert_eq!(called, tt.want_patch, "{name}: PATCH called");
        if called {
            let labels = patched_labels.lock().clone();
            assert_eq!(
                labels.get("adk_schema_version").map(String::as_str),
                Some(SCHEMA_VERSION),
                "{name}: patched labels = {labels:?}, want adk_schema_version={SCHEMA_VERSION:?}"
            );
        }
        if !tt.want_patched.is_empty() {
            assert_eq!(
                tt.want_patched,
                *patched_columns.lock(),
                "{name}: patched columns mismatch"
            );
        }
        if let Ok(p) = &result {
            close_plugin(p).await;
        }
    }
}

// parity: adapted: Go's mockTransport is a ScriptedClient that records unexpected requests (Go's t.Errorf inside the transport); conn.GetState() == Shutdown is the write client's closed flag
#[tokio::test]
async fn test_new_big_query_agent_analytics_plugin_table_setup() {
    let legacy_schema: BqSchema = events_schema()
        .into_iter()
        .filter(|f| f.name != EVENT_ID_FIELD)
        .collect();
    let legacy_fields = serde_json::to_value(TableSchema {
        fields: legacy_schema,
    })
    .expect("ToJSONFields");
    let legacy_table = json!({"etag": "etag-1", "schema": legacy_fields}).to_string();

    struct Case {
        name: &'static str,
        disable_upgrade: bool,
        // The status of each table metadata request in turn; 200 returns a table without
        // event_id.
        get_statuses: Vec<u16>,
        // The status of the table creation request.
        post_status: u16,
        want_gets: usize,
        want_post: bool,
        want_patch: bool,
        want_err: &'static str,
        // The status of the BigQuery API error that the construction error wraps.
        want_err_code: u16,
    }
    let tests = vec![
        Case {
            name: "metadata error other than 404 fails without creating the table",
            disable_upgrade: false,
            get_statuses: vec![403],
            post_status: 0,
            want_gets: 1,
            want_post: false,
            want_patch: false,
            want_err: "failed to get BigQuery table test-table metadata",
            want_err_code: 403,
        },
        Case {
            name: "409 from create reads the table again and upgrades it",
            disable_upgrade: false,
            get_statuses: vec![404, 200],
            post_status: 409,
            want_gets: 2,
            want_post: true,
            want_patch: true,
            want_err: "",
            want_err_code: 0,
        },
        Case {
            name: "409 from create with DisableSchemaUpgrade keeps the table as is",
            disable_upgrade: true,
            get_statuses: vec![404],
            post_status: 409,
            want_gets: 1,
            want_post: true,
            want_patch: false,
            want_err: "",
            want_err_code: 0,
        },
        Case {
            name: "create error other than 409 fails",
            disable_upgrade: false,
            get_statuses: vec![404],
            post_status: 403,
            want_gets: 1,
            want_post: true,
            want_patch: false,
            want_err: "failed to create BigQuery table test-table",
            want_err_code: 403,
        },
        Case {
            name: "metadata error after a 409 from create fails",
            disable_upgrade: false,
            get_statuses: vec![404, 403],
            post_status: 409,
            want_gets: 2,
            want_post: true,
            want_patch: false,
            want_err: "failed to get BigQuery table test-table metadata",
            want_err_code: 403,
        },
    ];

    const TABLE_PATH: &str = "/datasets/test_dataset/tables/test-table";
    for tt in tests {
        let name = tt.name;
        let mut config = table_config();
        config.disable_schema_upgrade = tt.disable_upgrade;
        // Views are created in the background once the table is ready; this test covers only
        // the table requests.
        config.disable_create_views = true;

        let gets = Arc::new(AtomicUsize::new(0));
        let posts = Arc::new(AtomicUsize::new(0));
        let patches = Arc::new(AtomicUsize::new(0));
        let patched_columns = Arc::new(parking_lot::Mutex::new(Vec::<String>::new()));
        let unexpected = Arc::new(parking_lot::Mutex::new(Vec::<String>::new()));
        let (g, po, pa, cols, unexp) = (
            Arc::clone(&gets),
            Arc::clone(&posts),
            Arc::clone(&patches),
            Arc::clone(&patched_columns),
            Arc::clone(&unexpected),
        );
        let (get_statuses, post_status, legacy) = (
            tt.get_statuses.clone(),
            tt.post_status,
            legacy_table.clone(),
        );
        let transport = ScriptedClient::new(move |r: &RecordedRequest| {
            if r.method == "GET" && r.path.ends_with(TABLE_PATH) {
                let n = g.fetch_add(1, Ordering::SeqCst) + 1;
                if n > get_statuses.len() {
                    unexp
                        .lock()
                        .push(format!("unexpected table metadata request #{n}"));
                    return api_error(400, "status 400");
                }
                let status = get_statuses[n - 1];
                if status != 200 {
                    return api_error(status, &format!("status {status}"));
                }
                return FakeResponse::json(legacy.clone());
            }
            if r.method == "POST" && r.path.ends_with("/datasets/test_dataset/tables") {
                po.fetch_add(1, Ordering::SeqCst);
                return api_error(post_status, &format!("status {post_status}"));
            }
            if (r.method == "PATCH" || r.method == "PUT") && r.path.ends_with(TABLE_PATH) {
                pa.fetch_add(1, Ordering::SeqCst);
                match serde_json::from_slice::<Value>(&r.body) {
                    Ok(body) => cols.lock().extend(body_field_names(&body)),
                    Err(e) => unexp.lock().push(format!("unmarshal patch request: {e}")),
                }
                return FakeResponse::json(r.body_text());
            }
            unexp
                .lock()
                .push(format!("unexpected request {} {}", r.method, r.path));
            api_error(400, "status 400")
        });
        let (_server, write_client) = new_test_write_client();
        let conn = write_client.clone();

        let result = new_bigquery_agent_analytics_plugin_with_clients(
            &Context::background(),
            config,
            Some(bq_client(transport)),
            Some(write_client),
        )
        .await;
        if !tt.want_err.is_empty() {
            let err = match &result {
                Err(err) if err.to_string().contains(tt.want_err) => err,
                _ => panic!(
                    "{name}: NewBigQueryAgentAnalyticsPluginWithClients() error = {:?}, want an error containing {:?}",
                    result.as_ref().err().map(ToString::to_string),
                    tt.want_err
                ),
            };
            assert!(
                has_api_code(err, tt.want_err_code),
                "{name}: NewBigQueryAgentAnalyticsPluginWithClients() error = {err}, want it to wrap a BigQuery API error with code {}",
                tt.want_err_code
            );
            assert!(
                is_shut_down(&conn),
                "{name}: write client connection state is not Shutdown after a failed construction"
            );
        } else if let Err(err) = &result {
            panic!("{name}: NewBigQueryAgentAnalyticsPluginWithClients() unexpected error: {err}");
        }
        assert_eq!(
            unexpected.lock().clone(),
            Vec::<String>::new(),
            "{name}: transport errors"
        );
        assert_eq!(
            gets.load(Ordering::SeqCst),
            tt.want_gets,
            "{name}: table metadata requests"
        );
        assert_eq!(
            posts.load(Ordering::SeqCst) > 0,
            tt.want_post,
            "{name}: table created"
        );
        assert_eq!(
            patches.load(Ordering::SeqCst) > 0,
            tt.want_patch,
            "{name}: table schema patched"
        );
        if tt.want_patch {
            let cols = patched_columns.lock().clone();
            assert!(
                cols.iter().any(|c| c == EVENT_ID_FIELD),
                "{name}: patched columns = {cols:?}, want them to include {EVENT_ID_FIELD}"
            );
        }
        if let Ok(p) = &result {
            close_plugin(p).await;
        }
    }
}

// parity: adapted: conn.GetState() == Shutdown is the write client's closed flag; Go's erroring transport is a ScriptedClient that records unexpected requests
#[tokio::test]
async fn test_new_big_query_agent_analytics_plugin_with_clients_closes_unused_write_client() {
    struct Case {
        name: &'static str,
        mutate: fn(&mut Config),
        // Empty when construction must succeed.
        want_err: &'static str,
    }
    let tests = [
        Case {
            name: "invalid BatchSize",
            mutate: |c| c.batch_size = -1,
            want_err: "BatchSize and QueueMaxSize must be >= 1",
        },
        Case {
            name: "invalid PayloadColumnDenylist",
            mutate: |c| c.payload_column_denylist = vec!["bogus".into()],
            want_err: r#"unsupported PayloadColumnDenylist entry "bogus""#,
        },
        Case {
            name: "disabled",
            mutate: |c| c.enabled = false,
            want_err: "",
        },
    ];
    for tt in tests {
        let name = tt.name;
        let mut config = table_config();
        (tt.mutate)(&mut config);
        let unexpected = Arc::new(parking_lot::Mutex::new(Vec::<String>::new()));
        let unexp = Arc::clone(&unexpected);
        let transport = ScriptedClient::new(move |r: &RecordedRequest| {
            unexp
                .lock()
                .push(format!("unexpected request {} {}", r.method, r.path));
            FakeResponse::new(500).fail("unexpected request")
        });
        let (_server, write_client) = new_test_write_client();
        let conn = write_client.clone();

        let result = new_bigquery_agent_analytics_plugin_with_clients(
            &Context::background(),
            config,
            Some(bq_client(transport)),
            Some(write_client),
        )
        .await;
        if !tt.want_err.is_empty() {
            match &result {
                Err(err) if err.to_string().contains(tt.want_err) => {}
                _ => panic!(
                    "{name}: NewBigQueryAgentAnalyticsPluginWithClients() error = {:?}, want an error containing {:?}",
                    result.as_ref().err().map(ToString::to_string),
                    tt.want_err
                ),
            }
        } else if let Err(err) = &result {
            panic!("{name}: NewBigQueryAgentAnalyticsPluginWithClients() = {err}, want a plugin");
        }
        assert_eq!(
            unexpected.lock().clone(),
            Vec::<String>::new(),
            "{name}: transport errors"
        );
        assert!(
            is_shut_down(&conn),
            "{name}: write client connection state is not Shutdown"
        );
        if let Ok(p) = &result {
            close_plugin(p).await;
        }
    }
}

// parity: adapted: Go's mockTransport is a ScriptedClient; Go's requests channel is the FakeWriteServer's first append request
#[tokio::test]
async fn test_payload_columns_projects_table_and_arrow_schema() {
    let mut config = table_config();
    config.payload_column_denylist = vec![
        COLUMN_CONTENT.into(),
        COLUMN_CONTENT_PARTS.into(),
        COLUMN_LATENCY_MS.into(),
    ];

    let created_columns = Arc::new(parking_lot::Mutex::new(Vec::<String>::new()));
    let created_labels = Arc::new(parking_lot::Mutex::new(BTreeMap::<String, String>::new()));
    let (cols, labels) = (Arc::clone(&created_columns), Arc::clone(&created_labels));
    let transport = ScriptedClient::new(move |r: &RecordedRequest| {
        if r.method == "GET" && r.path.contains("/datasets/test_dataset/tables/test-table") {
            return api_error(404, "Not found");
        }
        if r.method == "POST" && r.path.contains("/datasets/test_dataset/tables") {
            if let Ok(body) = serde_json::from_slice::<Value>(&r.body) {
                *labels.lock() = body_labels(&body);
                cols.lock().extend(body_field_names(&body));
            }
            return FakeResponse::json("{}");
        }
        if r.path.contains("/jobs") {
            return done_job();
        }
        FakeResponse::json("{}")
    });
    let server = FakeWriteServer::new();
    let p = new_bigquery_agent_analytics_plugin_with_clients(
        &Context::background(),
        config,
        Some(bq_client(transport)),
        Some(server.client().expect("write client")),
    )
    .await
    .unwrap_or_else(|e| panic!("NewBigQueryAgentAnalyticsPluginWithClients: {e}"));

    let want_columns: Vec<String> = [
        "timestamp",
        "event_id",
        "event_type",
        "agent",
        "session_id",
        "invocation_id",
        "user_id",
        "trace_id",
        "span_id",
        "parent_span_id",
        "attributes",
        "status",
        "error_message",
        "is_truncated",
    ]
    .map(String::from)
    .to_vec();
    assert_eq!(
        want_columns,
        *created_columns.lock(),
        "created table schema columns mismatch"
    );
    let want_labels: BTreeMap<String, String> =
        [("adk_schema_version".to_owned(), SCHEMA_VERSION.to_owned())].into();
    assert_eq!(
        want_labels,
        *created_labels.lock(),
        "created table labels mismatch"
    );

    let m_ctx = MockCtx::new();
    if let Err(err) =
        (p.on_user_message_callback().unwrap())(m_ctx.invocation(), text_content("user", "hello"))
            .await
    {
        panic!("OnUserMessageCallback: {err}");
    }
    (p.after_run_callback().unwrap())(m_ctx.invocation()).await;

    tokio::time::timeout(Duration::from_secs(5), server.wait_for_append_requests(1))
        .await
        .expect("an append request");
    let req = server.append_requests()[0].request.clone();
    assert_eq!(
        want_columns,
        arrow_writer_schema_columns(&req),
        "Arrow writer schema columns mismatch"
    );
    close_plugin(&p).await;
}

#[tokio::test]
async fn test_structured_content_parts_and_sanitized_text_in_callbacks() {
    let tp = new_test_plugin_with_config(|c| c.max_content_len = 60).await;
    let p = &tp.plugin;
    let m_ctx = &tp.ctx;
    let ctx = m_ctx.context();

    let structured_content = Content {
        role: "model".into(),
        parts: vec![
            Part::from_text(r#"{"api_key":"sk-secret-key","note":"hello world"}"#),
            Part {
                function_call: Some(FunctionCall {
                    id: "call-42".into(),
                    name: "lookup".into(),
                    args: obj(json!({"api_key": "sk-secret", "query": "weather"})),
                    ..FunctionCall::default()
                }),
                ..Part::default()
            },
            Part {
                function_response: Some(FunctionResponse {
                    id: "call-42".into(),
                    name: "lookup".into(),
                    response: obj(json!({"token": "tok-secret", "answer": "sunny"})),
                    ..FunctionResponse::default()
                }),
                ..Part::default()
            },
            Part {
                executable_code: Some(ExecutableCode {
                    language: Some(Language::Python),
                    code: "print(42)".into(),
                    ..ExecutableCode::default()
                }),
                ..Part::default()
            },
            Part {
                code_execution_result: Some(CodeExecutionResult {
                    outcome: Some(Outcome::Ok),
                    output: "42\n".into(),
                    ..CodeExecutionResult::default()
                }),
                ..Part::default()
            },
        ],
    };

    let mut errs = CallbackErrors::default();
    errs.add(
        (p.on_user_message_callback().unwrap())(
            m_ctx.invocation(),
            text_content("user", r#"{"api_key":"sk-user-secret","tail":"#),
        )
        .await,
    );
    errs.add(
        (p.before_model_callback().unwrap())(
            ctx.clone(),
            LlmRequest {
                model: "gemini-test".into(),
                contents: vec![text_content(
                    "user",
                    r#"{"api_key":"sk-req-secret","msg":"hi"}"#,
                )],
                ..LlmRequest::default()
            },
        )
        .await
        .1,
    );
    errs.add(
        (p.after_model_callback().unwrap())(
            ctx.clone(),
            LlmResponse {
                content: Some(structured_content),
                ..LlmResponse::default()
            },
            None,
        )
        .await
        .1,
    );
    let mut ev = Event {
        author: "test-agent".into(),
        ..Event::default()
    };
    ev.response.content = Some(text_content(
        "model",
        r#"{"api_key":"sk-secret-key","note":"hello world"}"#,
    ));
    errs.add((p.on_event_callback().unwrap())(m_ctx.invocation(), ev).await);
    errs.check();
    (p.after_run_callback().unwrap())(m_ctx.invocation()).await;

    let rows = tp
        .collect_rows(until_event_type(EVENT_INVOCATION_COMPLETED))
        .await;

    let user_row = only_row_of_type(&rows, EVENT_USER_MESSAGE_RECEIVED);
    let raw = str_of(&user_row, "content");
    assert!(
        !raw.contains("sk-user-secret"),
        "USER_MESSAGE_RECEIVED content {raw} leaked secret from malformed JSON"
    );
    assert_eq!(
        user_row.get("is_truncated"),
        Some(&RowValue::Bool(true)),
        "USER_MESSAGE_RECEIVED is_truncated, want true"
    );

    let req_row = only_row_of_type(&rows, EVENT_LLM_REQUEST);
    let raw = str_of(&req_row, "content");
    assert!(
        !raw.contains("sk-req-secret"),
        "LLM_REQUEST content {raw} leaked secret from JSON text"
    );

    let res_row = only_row_of_type(&rows, EVENT_LLM_RESPONSE);
    let raw = str_of(&res_row, "content");
    assert!(
        !raw.contains("sk-secret"),
        "LLM_RESPONSE content {raw} leaked secret"
    );
    let parts = parts_of(&res_row);
    assert_eq!(
        parts.len(),
        5,
        "LLM_RESPONSE content_parts len = {}, want 5: {parts:?}",
        parts.len()
    );
    assert_eq!(
        str_of(&parts[1], "mime_type"),
        "application/json",
        "LLM_RESPONSE parts[1] mime_type"
    );
    let attrs = str_of(&parts[1], "part_attributes");
    assert!(
        !attrs.contains("sk-secret") && attrs.contains(REDACTED_VALUE),
        "LLM_RESPONSE parts[1] part_attributes = {attrs}, want redacted args"
    );
    assert_eq!(
        str_of(&parts[2], "mime_type"),
        "application/json",
        "LLM_RESPONSE parts[2] mime_type"
    );
    let attrs = str_of(&parts[2], "part_attributes");
    assert!(
        !attrs.contains("tok-secret") && attrs.contains(REDACTED_VALUE),
        "LLM_RESPONSE parts[2] part_attributes = {attrs}, want redacted response"
    );
    assert!(
        str_of(&parts[3], "mime_type") == "text/plain" && str_of(&parts[3], "text") == "print(42)",
        "LLM_RESPONSE parts[3] = {:?}",
        parts[3]
    );
    assert!(
        str_of(&parts[4], "mime_type") == "text/plain" && str_of(&parts[4], "text") == "42\n",
        "LLM_RESPONSE parts[4] = {:?}",
        parts[4]
    );

    let agent_resp_row = only_row_of_type(&rows, EVENT_AGENT_RESPONSE);
    let raw = str_of(&agent_resp_row, "content");
    assert!(
        !raw.contains("sk-secret"),
        "AGENT_RESPONSE content {raw} leaked secret"
    );
    let resp_parts = parts_of(&agent_resp_row);
    assert_eq!(
        resp_parts.len(),
        1,
        "AGENT_RESPONSE content_parts len = {}, want 1: {resp_parts:?}",
        resp_parts.len()
    );
    let text = str_of(&resp_parts[0], "text");
    assert!(
        !text.contains("sk-secret"),
        "AGENT_RESPONSE parts[0] text = {text} leaked secret"
    );

    let prose_cases = [
        "[Urgent] please help me reset my router",
        "[1] First footnote",
        "[docs](https://example.com) has the details",
        "{{template}} rendered",
        "[x] task one",
        r#""see {this}" she said"#,
    ];
    for prose in prose_cases {
        let from = tp.server.append_requests().len();
        let mut prose_errs = CallbackErrors::default();
        prose_errs.add(
            (p.on_user_message_callback().unwrap())(
                m_ctx.invocation(),
                text_content("user", prose),
            )
            .await,
        );
        prose_errs.add(
            (p.after_model_callback().unwrap())(
                ctx.clone(),
                LlmResponse {
                    content: Some(text_content("model", prose)),
                    ..LlmResponse::default()
                },
                None,
            )
            .await
            .1,
        );
        prose_errs.check();
        (p.after_run_callback().unwrap())(m_ctx.invocation()).await;

        let prose_rows =
            collect_rows_from(&tp, from, until_event_type(EVENT_INVOCATION_COMPLETED)).await;
        let u_row = only_row_of_type(&prose_rows, EVENT_USER_MESSAGE_RECEIVED);
        let raw = str_of(&u_row, "content");
        assert!(
            !raw.contains(UNPARSEABLE_JSON_PLACEHOLDER) && raw.contains(&prose[..10]),
            "prose_{prose}: USER_MESSAGE_RECEIVED content = {raw}, want preserved prose {prose:?}"
        );
        assert_eq!(
            u_row.get("is_truncated"),
            Some(&RowValue::Bool(false)),
            "prose_{prose}: USER_MESSAGE_RECEIVED is_truncated, want false for prose {prose:?}"
        );
        let u_parts = parts_of(&u_row);
        assert!(
            u_parts.len() == 1 && str_of(&u_parts[0], "text") == prose,
            "prose_{prose}: USER_MESSAGE_RECEIVED content_parts = {u_parts:?}, want text {prose:?}"
        );
        let m_row = only_row_of_type(&prose_rows, EVENT_LLM_RESPONSE);
        let raw = str_of(&m_row, "content");
        assert!(
            !raw.contains(UNPARSEABLE_JSON_PLACEHOLDER),
            "prose_{prose}: LLM_RESPONSE content = {raw}, want preserved prose {prose:?}"
        );
        assert_eq!(
            m_row.get("is_truncated"),
            Some(&RowValue::Bool(false)),
            "prose_{prose}: LLM_RESPONSE is_truncated, want false for prose {prose:?}"
        );
    }
}

#[tokio::test]
async fn test_quoted_fragment_cost() {
    let tests = [
        (
            "broken fragments",
            format!(r#""a" {}{}"#, "\"x\n".repeat(64), "b".repeat(100_000)),
        ),
        // A key word lets the text through the credential pattern prefilter, so the patterns
        // run over all of it, for content and again for content_parts. At 50 KB that stays
        // well within the bound, while scanning the rest of the text again for each of the 64
        // fragments takes many times longer.
        (
            "broken fragments with a key word",
            format!(r#""a" {}{}"#, "\"token\n".repeat(64), "b".repeat(50_000)),
        ),
    ];
    for (name, text) in tests {
        let tp = setup_test_plugin().await;
        let start = Instant::now();
        let _ = (tp.plugin.on_user_message_callback().unwrap())(
            tp.ctx.invocation(),
            text_content("user", &text),
        )
        .await;
        let d = start.elapsed();
        assert!(
            d <= Duration::from_secs(3),
            "{name}: OnUserMessageCallback took {d:?} for a {} KB message",
            text.len() / 1000
        );
    }
}

#[tokio::test]
async fn test_quoted_json_secret_survives() {
    let tp = setup_test_plugin().await;
    let p = &tp.plugin;
    let m_ctx = &tp.ctx;
    let ctx = m_ctx.context();
    // Go fmt.Errorf("token exchange failed: %q", `{"access_token": "SECRET123"}`).
    let _ = (p.on_tool_error_callback().unwrap())(
        ctx.clone(),
        fake_tool("exchange"),
        no_args(),
        tool_error(r#"token exchange failed: "{\"access_token\": \"SECRET123\"}""#),
    )
    .await;
    (p.after_run_callback().unwrap())(m_ctx.invocation()).await;
    let rows = tp
        .collect_rows(until_event_type(EVENT_INVOCATION_COMPLETED))
        .await;
    let row = only_row_of_type(&rows, EVENT_TOOL_ERROR);
    let msg = str_of(&row, "error_message");
    assert!(!msg.contains("SECRET123"), "error_message = {msg}");
}

// parity: adapted: Go's fmt.Sprintf("%q", ...) / ("%+v", struct) texts are written out as the literals Go prints
#[tokio::test]
async fn test_credential_shapes_never_reach_a_row() {
    const SECRET: &str = "SECRET123";
    let tests: Vec<(&str, &str, &str)> = vec![
        (
            "JSON quoted with %q",
            r#"token exchange failed: "{\"access_token\": \"SECRET123\"}""#,
            r#"token exchange failed: "{\"access_token\":\"[REDACTED]\"}""#,
        ),
        (
            "value quoted with escaped quotes",
            r#"failed: password=\"SECRET123\""#,
            r#"failed: password=\"[REDACTED]\""#,
        ),
        (
            "single-quoted key",
            r"bad config: {'api_key': 'SECRET123'}",
            r"bad config: {'api_key':'[REDACTED]'}",
        ),
        (
            "credentials envelope is redacted whole",
            r#"auth failed: {"credentials": {"access_token": "SECRET123", "expires_in": 3600}}"#,
            r#"auth failed: {"credentials":"[REDACTED]"}"#,
        ),
        (
            "AWS STS credentials envelope",
            r#"assume role failed: {"Credentials": {"AccessKeyId": "EXAMPLEKEYID", "SecretAccessKey": "SECRET123", "SessionToken": "SECRET123", "Expiration": "2026-10-14T00:00:00Z"}}"#,
            r#"assume role failed: {"Credentials":"[REDACTED]"}"#,
        ),
        (
            "object under a sensitive key",
            r#"auth failed: {"token": {"value": "SECRET123"}}"#,
            r#"auth failed: {"token":"[REDACTED]"}"#,
        ),
        (
            "object quoted with %q whose string field holds a closing brace",
            r#"auth failed: "{\"token\": {\"label\": \"a},\", \"val\": \"SECRET123\"}}""#,
            r#"auth failed: "{\"token\":[REDACTED]}""#,
        ),
        (
            "bare value with a parenthesis",
            "failed: password=abc)SECRET123",
            "failed: password=[REDACTED]",
        ),
        (
            "bare value starting with a parenthesis",
            "failed: password=)SECRET123",
            "failed: password=[REDACTED]",
        ),
        (
            "bare value starting with slashes",
            "failed: password: //SECRET123",
            "failed: password:[REDACTED]",
        ),
        (
            "bare value with a closing brace",
            "failed: password=ab}SECRET123",
            "failed: password=[REDACTED]",
        ),
        (
            "bare value ending with a closing brace",
            "failed: password=SECRET123}",
            "failed: password=[REDACTED]",
        ),
        (
            "quoted value ending with a closing brace",
            r"bad config: {'password': 'SECRET123}'}",
            r"bad config: {'password':'[REDACTED]'}",
        ),
        (
            "camelCase keys",
            "auth failed: accessToken=SECRET123 apiKey: SECRET123",
            "auth failed: accessToken=[REDACTED] apiKey:[REDACTED]",
        ),
        (
            "Go struct printed with %+v",
            "bad config: {APIKey:SECRET123 ClientSecret:SECRET123 Region:us}",
            "bad config: {APIKey:[REDACTED] ClientSecret:[REDACTED] Region:us}",
        ),
        (
            "Go struct printed with %+v ending with credential fields",
            "bad config: {APIKey:SECRET123 ClientSecret:SECRET123}",
            "bad config: {APIKey:[REDACTED] ClientSecret:[REDACTED]}",
        ),
        (
            "Go struct printed with %+v whose credential field holds a closing brace",
            "bad config: {Password:ab}SECRET123}",
            "bad config: {Password:[REDACTED]}",
        ),
        (
            "nested Go struct printed with %+v",
            "bad config: {Auth:{Token:SECRET123}}",
            "bad config: {Auth:{Token:[REDACTED]}}",
        ),
        (
            "slice of Go structs printed with %+v",
            "bad config: [{APIKey:SECRET123}]",
            "bad config: [{APIKey:[REDACTED]}]",
        ),
        (
            "AWS credentials printed with %+v",
            "assume role failed: {AccessKeyID:EXAMPLEKEYID SecretAccessKey:SECRET123 SessionToken:SECRET123}",
            "assume role failed: {AccessKeyID:EXAMPLEKEYID SecretAccessKey:[REDACTED] SessionToken:[REDACTED]}",
        ),
        (
            "x_goog_api_key",
            "request failed: x_goog_api_key=SECRET123",
            "request failed: x_goog_api_key=[REDACTED]",
        ),
        (
            "session user key",
            "failed: user:api_key=SECRET123",
            "failed: user:api_key=[REDACTED]",
        ),
        (
            "embedded JSON with a camelCase key",
            r#"token exchange failed: {"accessToken": "SECRET123", "expiresIn": 3600}"#,
            r#"token exchange failed: {"accessToken":"[REDACTED]", "expiresIn": 3600}"#,
        ),
        (
            "command-line flag",
            "mysql -u root --password=SECRET123 failed",
            "mysql -u root --password=[REDACTED] failed",
        ),
        (
            "command-line flag with a hyphenated key",
            "--api-key=SECRET123",
            "--api-key=[REDACTED]",
        ),
        (
            "hyphenated header ending in token",
            "X-Auth-Token: SECRET123",
            "X-Auth-Token:[REDACTED]",
        ),
        (
            "upper-case hyphenated header ending in token",
            "PRIVATE-TOKEN: SECRET123",
            "PRIVATE-TOKEN:[REDACTED]",
        ),
        (
            "credentials block in YAML",
            "config:\ncredentials:\n  token: SECRET123\n  region: us",
            "config:\ncredentials:\n  token:[REDACTED]\n  region: us",
        ),
        (
            "quoted value that starts with a key-value pair",
            r"bad config: password: 'token: SECRET123 SECRET123'",
            r"bad config: password:'[REDACTED]'",
        ),
        (
            "credentials in a Python dict whose value starts with a key-value pair",
            r"bad config: {'credentials': 'secret=correct horse SECRET123 staple'}",
            r"bad config: {'credentials':'[REDACTED]'}",
        ),
        (
            "bare value that ends with a key-value pair",
            "failed: password=SECRET123-token:SECRET123",
            "failed: password=[REDACTED]",
        ),
        (
            "key-shaped bare value with no value after it",
            "failed: password=SECRET123-token:",
            "failed: password=[REDACTED]",
        ),
        (
            "key-shaped bare value followed by a closing brace",
            "failed: password=SECRET123-token: }",
            "failed: password=[REDACTED] }",
        ),
        (
            "key-shaped value of a Go struct field followed by the closing brace",
            "failed: {Password:SECRET123-token: }",
            "failed: {Password:[REDACTED] }",
        ),
        (
            "base64 value that ends with a key name, one pad and a closing brace",
            "failed: api_key=SECRET123AbC+xYz/Sig= }",
            "failed: api_key=[REDACTED] }",
        ),
        (
            "key-shaped bare value followed by a closing bracket",
            "failed: password=SECRET123-token: ]",
            "failed: password=[REDACTED] ]",
        ),
        (
            "key-shaped value of a Go struct field followed by the closing brace and another struct",
            "failed: [{Password:SECRET123-token: } {User:bob}]",
            "failed: [{Password:[REDACTED] } {User:bob}]",
        ),
        (
            "nested key whose value starts with a closing bracket",
            "failed: credentials: token = ]SECRET123",
            "failed: credentials: token=[REDACTED]",
        ),
        (
            "nested key whose quoted value has leading whitespace",
            r#"failed: credentials: token: " SECRET123""#,
            r#"failed: credentials: token:"[REDACTED]""#,
        ),
        (
            "nested key in a Go struct whose quoted value has leading whitespace",
            r#"failed: {Credentials:token: " SECRET123"}"#,
            r#"failed: {Credentials:token:"[REDACTED]"}"#,
        ),
        (
            "key-shaped value of a Go struct field followed by an unclosed quote",
            r#"failed: {Password:SECRET123-token: "}"#,
            r#"failed: {Password:[REDACTED] "}"#,
        ),
        (
            "nested key glued to a quoted value with spaces",
            r#"failed: credentials: password="correct SECRET123 battery""#,
            r#"failed: credentials: password="[REDACTED]""#,
        ),
        (
            "nested key in a Go struct glued to a quoted value with leading whitespace",
            r#"failed: {Credentials:token=" SECRET123"}"#,
            r#"failed: {Credentials:token="[REDACTED]"}"#,
        ),
    ];
    for (name, text, want) in tests {
        let tp = setup_test_plugin().await;
        let p = &tp.plugin;
        let m_ctx = &tp.ctx;
        let ctx = m_ctx.context();
        let mut errs = CallbackErrors::default();
        errs.add(
            (p.on_user_message_callback().unwrap())(m_ctx.invocation(), text_content("user", text))
                .await,
        );
        errs.add(
            (p.after_tool_callback().unwrap())(
                ctx.clone(),
                fake_tool("lookup"),
                no_args(),
                obj(json!({"output": text})),
                None,
            )
            .await,
        );
        errs.add(
            (p.on_tool_error_callback().unwrap())(
                ctx.clone(),
                fake_tool("exchange"),
                no_args(),
                tool_error(text),
            )
            .await,
        );
        errs.check();
        (p.after_run_callback().unwrap())(m_ctx.invocation()).await;

        let rows = tp
            .collect_rows(until_event_type(EVENT_INVOCATION_COMPLETED))
            .await;
        let user_row = only_row_of_type(&rows, EVENT_USER_MESSAGE_RECEIVED);
        let content = json_object_column(&user_row, "content");
        let got = lookup(&content, &["text_summary"]);
        assert_eq!(
            got.and_then(Value::as_str),
            Some(want),
            "{name}: USER_MESSAGE_RECEIVED content.text_summary = {got:?}, want {want:?}"
        );
        let parts = parts_of(&user_row);
        assert!(
            parts.len() == 1 && str_of(&parts[0], "text") == want,
            "{name}: USER_MESSAGE_RECEIVED content_parts = {parts:?}, want text {want:?}"
        );
        let tool_row = only_row_of_type(&rows, EVENT_TOOL_COMPLETED);
        let content = json_object_column(&tool_row, "content");
        let got = lookup(&content, &["result", "output"]);
        assert_eq!(
            got.and_then(Value::as_str),
            Some(want),
            "{name}: TOOL_COMPLETED content.result.output = {got:?}, want {want:?}"
        );
        assert_eq!(
            str_of(&only_row_of_type(&rows, EVENT_TOOL_ERROR), "error_message"),
            want,
            "{name}: TOOL_ERROR error_message"
        );
        for row in &rows {
            let s = format!("{row:?}");
            assert!(
                !s.contains(SECRET),
                "{name}: {:?} row leaked {SECRET}: {s}",
                row.get("event_type")
            );
        }
    }
}
