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

//! Part of the port of ADK-Go `plugin/agentanalytics/bigquery_agent_analytics_plugin_test.go`
//! (owner: Task 10): the 30 Go tests from `TestNewBigQueryAgentAnalyticsPlugin_Disabled` to
//! `TestToolFailure_WritesErrorRows` in the Go file's order (the exact list is in the 7b-3a plan's
//! contracts, Appendix A).
//!
//! Go's direct callback tests call the plugin's callbacks with one `mockInvocationContext`;
//! here every callback gets a context of one [`MockCtx`], built once
//! ([`Mc`]), so all of them share one agent and activation as Go's `mCtx`. Go puts an OTel span
//! context in `mCtx.ctx`; gadk's callbacks read the ambient span from the tracing subscriber
//! (`gadk_engine::telemetry::current_span_ids`), which a test cannot set without a
//! `tracing-subscriber` registry, so the in-span cases call the plugin's callback bodies with a
//! `CallbackCtx::with_span` ([`SpanPlugin`]).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use chrono::Utc;
use gadk_agent::{AgentError, ContextDelta, InvocationContext, Tool, ToolError};
use gadk_engine::telemetry::d6::SpanIds;
use gadk_genai::{
    Blob, CodeExecutionResult, Content, ExecutableCode, FileData, FinishReason, FunctionCall,
    FunctionResponse, GenerateContentConfig, GenerateContentResponseUsageMetadata, Part,
};
use gadk_model::rest::test_util::{FakeResponse, RecordedRequest, ScriptedClient};
use gadk_model::{LlmRequest, LlmResponse, ModelError};
use gadk_session::{Event, EventActions, NodeInfo};
use pretty_assertions::assert_eq;
use regex::Regex;
use serde_json::{Map, Value, json};

use super::helpers::{CallbackErrors, FakeTool, check_json_at, fake_tool, parts_of, text_content};
use super::support::{
    MockCtx, TestPlugin, check_json, event_types, json_column, json_object_column, lookup,
    new_test_plugin_with_config, only_row_of_type, plugin_of, setup_test_plugin, test_config,
    until_event_type, until_invocation_end,
};
use crate::batch_processor::BatchProcessor;
use crate::bigquery::BigQueryClient;
use crate::bigquery::fake::RecordingLogger;
use crate::config::{ClientOptions, Config, ContentFormatter};
use crate::context::Context;
use crate::event_types::*;
use crate::plugin::callbacks::has_generated_content;
use crate::plugin::{
    AnalyticsPlugin, PLUGIN_NAME, new_bigquery_agent_analytics_plugin_with_clients,
    new_bigquery_agent_analytics_plugin_with_config,
};
use crate::redaction::{
    COLUMN_ATTRIBUTES, COLUMN_CONTENT, COLUMN_CONTENT_PARTS, COLUMN_LATENCY_MS,
    CustomMetadataFilter, REDACTED_VALUE,
};
use crate::row::{Row, RowValue};
use crate::schema::{FIELD_TYPE_STRING, arrow_schema, events_schema};
use crate::testutil::fake_write_server::FakeWriteServer;
use crate::trace::{CallbackCtx, TraceManager};

// ---------------------------------------------------------------------------------------------
// File-local helpers (Go's test helpers of this file)
// ---------------------------------------------------------------------------------------------

/// Go `eventIDPattern`: the 32 lowercase hex digits of adk-python's `uuid4().hex`.
static EVENT_ID_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[0-9a-f]{32}$").expect("regex"));

/// Go `spanIDPattern` (trace_manager_test.go).
static SPAN_ID_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[0-9a-f]{16}$").expect("regex"));

/// Go `textPart(text, index)`: the `content_parts` entry of a text part, as decoded from Arrow.
fn text_part(text: &str, index: i64) -> Row {
    Row::from([
        ("mime_type".to_owned(), RowValue::Str("text/plain".into())),
        ("text".to_owned(), RowValue::Str(text.into())),
        ("part_index".to_owned(), RowValue::Int(index)),
        ("storage_mode".to_owned(), RowValue::Str("INLINE".into())),
    ])
}

/// Go's one `mCtx` handed to every callback: the ADK contexts of a [`MockCtx`], built once.
/// The callback context derives from the invocation context by a path / function-call delta
/// only, so both share one agent, session and activation, as Go's single `mCtx` (a context
/// built per call would be a new activation, which the trace manager keys calls by).
struct Mc {
    inv: InvocationContext,
    ctx: gadk_agent::Context,
}

impl Mc {
    fn new(m: &MockCtx) -> Mc {
        let inv = m.invocation();
        let ctx = inv.clone().into_context();
        let ctx = if m.function_call_id.is_empty() {
            ctx
        } else {
            ctx.with_delta(ContextDelta {
                function_call_id: Some(m.function_call_id.clone()),
                ..ContextDelta::default()
            })
        };
        Mc { inv, ctx }
    }

    /// Go's `mCtx` passed to an invocation callback.
    fn invocation(&self) -> InvocationContext {
        self.inv.clone()
    }

    /// Go's `mCtx` passed to an agent / model / tool callback.
    fn context(&self) -> gadk_agent::Context {
        self.ctx.clone()
    }
}

/// Go `checkJSON(t, name, got, want)`.
fn check_json_eq(name: &str, got: &Value, want: &str) {
    check_json(name, got, want).unwrap_or_else(|e| panic!("{e}"));
}

/// A string column of `row`.
fn str_of(row: &Row, column: &str) -> Option<String> {
    match row.get(column) {
        Some(RowValue::Str(s)) => Some(s.clone()),
        _ => None,
    }
}

/// Go `jsonObjectColumn(t, row, "content")` of a row that must have content.
fn content_of(row: &Row) -> Value {
    json_object_column(row, COLUMN_CONTENT)
}

/// Go `bq.NewClient(ctx, "test-project", option.WithHTTPClient(&http.Client{Transport:
/// mockTransport}))` with `roundTrip` as the transport.
fn bq_client_with(
    round_trip: impl Fn(&RecordedRequest) -> FakeResponse + Send + Sync + 'static,
) -> Arc<BigQueryClient> {
    let options = ClientOptions {
        http_client: Some(ScriptedClient::new(round_trip)),
        without_authentication: true,
        endpoint: Some("http://bq.test/bigquery/v2/".into()),
        ..ClientOptions::default()
    };
    Arc::new(BigQueryClient::new("test-project", &options).expect("bigquery client"))
}

/// Go `p.AfterModelCallback()(mCtx, res, err)`'s error.
async fn after_model(
    tp: &TestPlugin,
    ctx: &Mc,
    res: LlmResponse,
    err: Option<ModelError>,
) -> Result<Option<LlmResponse>, AgentError> {
    (tp.plugin.after_model_callback().unwrap())(ctx.context(), res, err.map(Arc::new))
        .await
        .1
}

/// Go `p.AfterRunCallback()(mCtx)`.
async fn after_run(tp: &TestPlugin, ctx: &Mc) {
    (tp.plugin.after_run_callback().unwrap())(ctx.invocation()).await;
}

/// The plugin of Go's in-span cases: the production callback bodies over a processor that
/// writes to a [`FakeWriteServer`], as `newTestPluginWithConfig` builds it, called with a
/// [`CallbackCtx`] that carries Go's ambient span context.
struct SpanPlugin {
    ap: Arc<AnalyticsPlugin>,
    tp: TestPlugin,
}

impl SpanPlugin {
    async fn new(mutate: impl FnOnce(&mut Config)) -> SpanPlugin {
        let mut config = test_config();
        mutate(&mut config);
        let config = config.apply_defaults();
        let server = FakeWriteServer::new();
        let stream = format!(
            "projects/{}/datasets/{}/tables/{}/streams/_default",
            config.project_id, config.dataset_id, config.table_name
        );
        let processor = BatchProcessor::new(
            &Context::background(),
            Some(server.client().expect("write client")),
            &stream,
            config.clone(),
        )
        .await
        .expect("batch processor");
        processor.start();
        let ap = Arc::new(AnalyticsPlugin {
            custom_metadata: CustomMetadataFilter::new(&config.custom_metadata_allowlist),
            config,
            processor: Some(processor),
            traces: Some(Arc::new(TraceManager::new())),
            ..AnalyticsPlugin::default()
        });
        let tp = TestPlugin {
            plugin: plugin_of(PLUGIN_NAME, &ap),
            server,
            ctx: MockCtx::new(),
        };
        SpanPlugin { ap, tp }
    }

    /// Go's `mCtx` with `trace.ContextWithSpanContext(ctx, sc)` when `sc` is set.
    fn ctx(&self, sc: Option<SpanIds>) -> CallbackCtx {
        let ctx = CallbackCtx::from_invocation(&self.tp.ctx.invocation());
        match sc {
            Some(sc) => ctx.with_span(sc),
            None => ctx,
        }
    }

    /// Go's `OnUserMessageCallback` then `AfterRunCallback` (with its run-end flush).
    async fn user_message_then_after_run(&self, sc: Option<SpanIds>, msg: &Content) {
        let ctx = self.ctx(sc);
        self.ap.on_user_message(&ctx, msg);
        self.ap.after_run(&ctx);
        self.ap.processor.as_ref().unwrap().flush_run_end().await;
    }
}

/// Go `trace.TraceIDFromHex` / `SpanIDFromHex` of a span context.
fn span_ids(trace_hex: &str, span_hex: &str) -> SpanIds {
    fn bytes<const N: usize>(hex: &str) -> [u8; N] {
        let mut out = [0u8; N];
        for (i, b) in out.iter_mut().enumerate() {
            *b = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).expect("hex");
        }
        out
    }
    SpanIds {
        trace_id: bytes(trace_hex),
        span_id: bytes(span_hex),
    }
}

// ---------------------------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn test_new_big_query_agent_analytics_plugin_disabled() {
    let config = Config {
        enabled: false,
        ..Config::default()
    };
    let p = new_bigquery_agent_analytics_plugin_with_config(&Context::background(), config)
        .await
        .unwrap_or_else(|e| panic!("Expected no error, got {e}"));
    assert_eq!(
        p.name(),
        "bigquery_agent_analytics",
        "Expected name 'bigquery_agent_analytics'"
    );
}

#[tokio::test]
async fn test_new_big_query_agent_analytics_plugin_enabled() {
    let tp = setup_test_plugin().await;
    let p = &tp.plugin;
    assert_eq!(
        p.name(),
        "bigquery_agent_analytics",
        "Expected name 'bigquery_agent_analytics'"
    );
    // Verify callbacks are registered
    assert!(
        p.on_user_message_callback().is_some(),
        "OnUserMessageCallback is nil"
    );
    assert!(
        p.before_run_callback().is_some(),
        "BeforeRunCallback is nil"
    );
    assert!(p.after_run_callback().is_some(), "AfterRunCallback is nil");
    assert!(p.on_event_callback().is_some(), "OnEventCallback is nil");
}

#[tokio::test]
async fn test_on_user_message_callback_appends_to_writer() {
    let tp = setup_test_plugin().await;
    let m = Mc::new(&tp.ctx.clone());
    let user_msg_cb = tp
        .plugin
        .on_user_message_callback()
        .expect("OnUserMessageCallback is nil");
    let msg = Content {
        parts: vec![Part::from_text("hello")],
        ..Content::default()
    };
    user_msg_cb(m.invocation(), msg)
        .await
        .unwrap_or_else(|e| panic!("OnUserMessageCallback error: {e}"));
    after_run(&tp, &m).await;

    // Verify that a request was received
    tokio::time::timeout(
        Duration::from_secs(5),
        tp.server.wait_for_append_requests(1),
    )
    .await
    .expect("Timed out waiting for request");
}

#[tokio::test]
async fn test_before_run_callback_appends_to_writer() {
    let tp = setup_test_plugin().await;
    let m = Mc::new(&tp.ctx.clone());
    let before_run_cb = tp
        .plugin
        .before_run_callback()
        .expect("BeforeRunCallback is nil");
    before_run_cb(m.invocation())
        .await
        .unwrap_or_else(|e| panic!("BeforeRunCallback error: {e}"));
    after_run(&tp, &m).await;

    tokio::time::timeout(
        Duration::from_secs(5),
        tp.server.wait_for_append_requests(1),
    )
    .await
    .expect("Timed out waiting for request");
}

#[tokio::test]
async fn test_on_event_callback_appends_to_writer() {
    let tp = setup_test_plugin().await;
    let m = Mc::new(&tp.ctx.clone());
    let event_cb = tp
        .plugin
        .on_event_callback()
        .expect("OnEventCallback is nil");
    let ev = Event {
        author: "test-author".into(),
        response: LlmResponse {
            content: Some(Content {
                parts: vec![Part::from_text("event-data")],
                ..Content::default()
            }),
            ..LlmResponse::default()
        },
        ..Event::default()
    };
    event_cb(m.invocation(), ev)
        .await
        .unwrap_or_else(|e| panic!("OnEventCallback error: {e}"));
    after_run(&tp, &m).await;

    tokio::time::timeout(
        Duration::from_secs(5),
        tp.server.wait_for_append_requests(1),
    )
    .await
    .expect("Timed out waiting for request");
}

#[tokio::test]
async fn test_on_model_error_callback_appends_to_writer() {
    let tp = setup_test_plugin().await;
    let m = Mc::new(&tp.ctx.clone());
    let model_error_cb = tp
        .plugin
        .on_model_error_callback()
        .expect("OnModelErrorCallback is nil");
    model_error_cb(
        m.context(),
        LlmRequest::default(),
        Arc::new(ModelError::Other("test error".into())),
    )
    .await
    .unwrap_or_else(|e| panic!("OnModelErrorCallback error: {e}"));
    after_run(&tp, &m).await;

    tokio::time::timeout(
        Duration::from_secs(5),
        tp.server.wait_for_append_requests(1),
    )
    .await
    .expect("Timed out waiting for request");
}

#[tokio::test]
async fn test_after_model_callback_partial_chunks_do_not_each_log_usage() {
    async fn run(chunks: Vec<LlmResponse>) -> usize {
        let tp = setup_test_plugin().await;
        let m = Mc::new(&tp.ctx.clone());
        for c in chunks {
            after_model(&tp, &m, c, None)
                .await
                .unwrap_or_else(|e| panic!("AfterModelCallback error: {e}"));
        }
        after_run(&tp, &m).await; // logs 1 INVOCATION_END row too, then flushes
        // Go reads requests until none arrives for a second.
        loop {
            let n = tp.server.append_requests().len();
            if tokio::time::timeout(
                Duration::from_secs(1),
                tp.server.wait_for_append_requests(n + 1),
            )
            .await
            .is_err()
            {
                return tp.rows().len();
            }
        }
    }
    let usage = |c: i32| {
        Some(GenerateContentResponseUsageMetadata {
            prompt_token_count: 100,
            candidates_token_count: c,
            ..Default::default()
        })
    };
    let baseline = run(vec![LlmResponse {
        partial: false,
        content: Some(Content {
            parts: vec![Part::from_text("final")],
            ..Content::default()
        }),
        usage_metadata: usage(15),
        ..LlmResponse::default()
    }])
    .await;
    let partial = |c: i32, partial: bool| LlmResponse {
        partial,
        usage_metadata: usage(c),
        ..LlmResponse::default()
    };
    let streaming = run(vec![
        partial(5, true),
        partial(10, true),
        partial(15, true),
        partial(15, false),
    ])
    .await;
    assert_eq!(
        streaming, baseline,
        "expected streaming turn to log the same number of rows as the baseline turn ({baseline}), got {streaming}"
    );
}

// parity: adapted: Go's "nil response" and "nil part" cases have no gadk form (an `LlmResponse` and a `Part` are values); the other 13 cases are Go's
#[test]
fn test_has_generated_content() {
    let content = |parts: Vec<Part>| LlmResponse {
        partial: true,
        content: Some(Content {
            role: "model".into(),
            parts,
        }),
        ..LlmResponse::default()
    };
    let part = |f: fn(&mut Part)| {
        let mut p = Part::default();
        f(&mut p);
        p
    };
    let tests: Vec<(&str, LlmResponse, bool)> = vec![
        (
            "usage only",
            LlmResponse {
                partial: true,
                usage_metadata: Some(GenerateContentResponseUsageMetadata {
                    prompt_token_count: 10,
                    ..Default::default()
                }),
                ..LlmResponse::default()
            },
            false,
        ),
        (
            "no content",
            LlmResponse {
                partial: true,
                ..LlmResponse::default()
            },
            false,
        ),
        (
            "role only",
            LlmResponse {
                partial: true,
                content: Some(Content {
                    role: "model".into(),
                    parts: Vec::new(),
                }),
                ..LlmResponse::default()
            },
            false,
        ),
        ("empty parts", content(Vec::new()), false),
        ("empty part", content(vec![Part::default()]), false),
        ("text", content(vec![Part::from_text("Hel")]), true),
        (
            "inline data",
            content(vec![part(|p| {
                p.inline_data = Some(Blob {
                    mime_type: "image/png".into(),
                    data: vec![1u8].into(),
                    ..Default::default()
                });
            })]),
            true,
        ),
        (
            "file data",
            content(vec![part(|p| {
                p.file_data = Some(FileData {
                    file_uri: "gs://bucket/object".into(),
                    mime_type: "image/png".into(),
                    ..Default::default()
                });
            })]),
            true,
        ),
        (
            "function call",
            content(vec![part(|p| {
                p.function_call = Some(FunctionCall {
                    name: "lookup".into(),
                    ..Default::default()
                });
            })]),
            true,
        ),
        (
            "function response",
            content(vec![part(|p| {
                p.function_response = Some(FunctionResponse {
                    name: "lookup".into(),
                    ..Default::default()
                });
            })]),
            true,
        ),
        (
            "executable code",
            content(vec![part(|p| {
                p.executable_code = Some(ExecutableCode {
                    code: "print(1)".into(),
                    ..Default::default()
                });
            })]),
            true,
        ),
        (
            "code execution result",
            content(vec![part(|p| {
                p.code_execution_result = Some(CodeExecutionResult {
                    output: "1".into(),
                    ..Default::default()
                });
            })]),
            true,
        ),
        (
            "text after empty parts",
            content(vec![Part::default(), Part::from_text("Hel")]),
            true,
        ),
    ];
    for (name, res, want) in tests {
        assert_eq!(
            has_generated_content(&res),
            want,
            "{name}: hasGeneratedContent() = {}, want {want}",
            !want
        );
    }
}

// parity: adapted: Go's span context in mCtx.ctx → the callback bodies called with CallbackCtx::with_span (gadk reads the ambient span from the tracing subscriber)
#[tokio::test]
async fn test_log_event_extracts_trace_info() {
    let sp = SpanPlugin::new(|_| {}).await;
    let sc = span_ids("4bf92f3577b34da6a3ce929d0e0e4736", "00f067aa0ba902b7");
    let msg = Content {
        parts: vec![Part::from_text("hello")],
        ..Content::default()
    };
    sp.user_message_then_after_run(Some(sc), &msg).await;

    tokio::time::timeout(
        Duration::from_secs(5),
        sp.tp.server.wait_for_append_requests(1),
    )
    .await
    .expect("Timed out waiting for request");
}

#[tokio::test]
async fn test_new_big_query_agent_analytics_plugin_create_table_with_partitioning() {
    let config = test_config();
    let create_called = Arc::new(parking_lot::Mutex::new(None::<String>));
    let seen = Arc::clone(&create_called);
    let bq_client = bq_client_with(move |r| {
        // Table metadata request: returns 404 Not Found to trigger creation
        if r.method == "GET" && r.path.contains("/datasets/test_dataset/tables/test-table") {
            return FakeResponse::json_status(
                404,
                r#"{"error":{"code":404,"message":"Not found"}}"#,
            );
        }
        // Table creation request
        if r.method == "POST" && r.path.contains("/datasets/test_dataset/tables") {
            *seen.lock() = Some(String::from_utf8_lossy(&r.body).into_owned());
            return FakeResponse::json("{}");
        }
        if r.path.contains("/jobs") {
            return FakeResponse::json(
                r#"{"jobReference":{"projectId":"test-project","jobId":"test-job"},"status":{"state":"DONE"}}"#,
            );
        }
        FakeResponse::json("{}")
    });
    let server = FakeWriteServer::new();
    let p = new_bigquery_agent_analytics_plugin_with_clients(
        &Context::background(),
        config,
        Some(bq_client),
        Some(server.client().unwrap()),
    )
    .await
    .unwrap_or_else(|e| panic!("Plugin initialization error: {e}"));

    let request_body = create_called
        .lock()
        .clone()
        .expect("Expected table creation to be called");
    let got: Value = serde_json::from_str(&request_body)
        .unwrap_or_else(|e| panic!("Failed to parse create-table request body: {e}"));
    let tp = got.get("timePartitioning").unwrap_or_else(|| {
        panic!("Expected request body to contain timePartitioning, got: {request_body}")
    });
    assert_eq!(tp["field"], "timestamp", "timePartitioning.field");
    assert_eq!(tp["type"], "DAY", "timePartitioning.type");
    (p.close().unwrap())().await.unwrap();
}

#[test]
fn test_events_schema_column_order() {
    let want = vec![
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
    ];
    let bq_columns: Vec<String> = events_schema().into_iter().map(|f| f.name).collect();
    assert_eq!(want, bq_columns, "EventsSchema() columns mismatch");
    let arrow_columns: Vec<String> = arrow_schema()
        .fields()
        .iter()
        .map(|f| f.name().clone())
        .collect();
    assert_eq!(want, arrow_columns, "ArrowSchema() columns mismatch");
    assert_eq!(
        events_schema()[1].field_type,
        FIELD_TYPE_STRING,
        "event_id type"
    );
}

#[test]
fn test_config_table_name_default() {
    let tests = [
        ("DefaultConfig", Config::default(), "agent_events"),
        (
            "empty TableName",
            Config::zero().apply_defaults(),
            "agent_events",
        ),
        (
            "explicit TableName",
            Config {
                table_name: "custom".into(),
                ..Config::zero()
            }
            .apply_defaults(),
            "custom",
        ),
    ];
    for (name, config, want) in tests {
        assert_eq!(config.table_name, want, "{name}: TableName");
    }
}

#[test]
fn test_config_default_on_features() {
    let cleared = Config {
        log_multi_modal_content: false,
        disable_session_metadata: true,
        disable_sensitive_key_redaction: true,
        disable_flush_on_run_end: true,
        disable_create_views: true,
        ..Config::default()
    };
    let disabled = Config {
        disable_multi_modal_content: true,
        disable_session_metadata: true,
        disable_sensitive_key_redaction: true,
        disable_flush_on_run_end: true,
        disable_create_views: true,
        ..Config::zero()
    };
    let default_disabled = Config {
        disable_multi_modal_content: true,
        disable_session_metadata: true,
        disable_sensitive_key_redaction: true,
        disable_flush_on_run_end: true,
        disable_create_views: true,
        ..Config::default()
    };
    let tests = [
        ("DefaultConfig", Config::default(), true),
        (
            "Config literal with LogMultiModalContent",
            Config {
                log_multi_modal_content: true,
                ..Config::zero()
            },
            true,
        ),
        ("DefaultConfig with the features cleared", cleared, false),
        ("Config literal with Disable fields", disabled, false),
        ("DefaultConfig with Disable fields", default_disabled, false),
    ];
    for (name, config, want) in tests {
        let got = [
            ("logMultiModalContent", config.log_multi_modal_content()),
            ("logSessionMetadata", config.log_session_metadata()),
            ("redactSensitiveKeys", config.redact_sensitive_keys()),
            ("flushOnRunEnd", config.flush_on_run_end()),
            ("createViews", config.create_views()),
        ];
        for (f, enabled) in got {
            assert_eq!(enabled, want, "{name}: {f}() = {enabled}, want {want}");
        }
    }
}

/// One expected row of [`test_callbacks_write_rows`].
#[derive(Default)]
struct WantRow {
    event_type: &'static str,
    status: &'static str,
    err_msg: &'static str,
    /// The JSON of the content column; empty if the row has none.
    content: &'static str,
    parts: Vec<Row>,
    /// Dot-separated attributes paths and their JSON, on top of the attributes every row has.
    attrs: Vec<(&'static str, &'static str)>,
}

#[tokio::test]
async fn test_callbacks_write_rows() {
    let tp = new_test_plugin_with_config(|c| {
        c.custom_tags = json!({"team": "search", "labels": {"tier": "gold"}})
            .as_object()
            .unwrap()
            .clone();
    })
    .await;
    let mut m = tp.ctx.clone();
    m.app_name = "test-app".into();
    m.branch = "root.test-agent".into();
    m.function_call_id = "call-1".into();
    m.state = json!({"user_pref": "dark", "temp:scratch": "scratch-value", "api_key": "sk-test"})
        .as_object()
        .unwrap()
        .clone();
    let m = Mc::new(&m);

    let lookup_tool: Arc<dyn Tool> = Arc::new(FakeTool {
        name: "lookup".into(),
        description: "Looks things up".into(),
        long_running: true,
        ..FakeTool::default()
    });
    let tool_args = json!({"query": "weather", "big": 9_007_199_254_740_993_i64})
        .as_object()
        .unwrap()
        .clone();
    let llm_request = LlmRequest {
        model: "gemini-test".into(),
        contents: vec![text_content("user", "hello")],
        config: Some(GenerateContentConfig {
            system_instruction: Some(text_content("", "be brief")),
            temperature: Some(0.5),
            labels: BTreeMap::from([("env".to_owned(), "test".to_owned())]),
            ..Default::default()
        }),
        ..LlmRequest::default()
    };
    let llm_response = LlmResponse {
        content: Some(text_content("model", "hi there")),
        usage_metadata: Some(GenerateContentResponseUsageMetadata {
            prompt_token_count: 10,
            candidates_token_count: 5,
            total_token_count: 15,
            ..Default::default()
        }),
        model_version: "gemini-test-001".into(),
        finish_reason: Some(FinishReason::Stop),
        turn_complete: true,
        ..LlmResponse::default()
    };
    let ev = Event {
        id: "event-1".into(),
        author: "test-agent".into(),
        branch: "root.test-agent".into(),
        isolation_scope: "wf@0/review@2".into(),
        node_info: Some(NodeInfo {
            path: "wf@0/review@2".into(),
            output_for: vec!["wf@0".into()],
            ..Default::default()
        }),
        routes: vec!["approve".into()],
        actions: EventActions {
            artifact_delta: Some(BTreeMap::from([("report.txt".to_owned(), 3)])),
            escalate: true,
            ..Default::default()
        },
        response: LlmResponse {
            content: Some(text_content("model", "event text")),
            ..LlmResponse::default()
        },
        ..Event::default()
    };

    let p = &tp.plugin;
    let start = Utc::now();
    let mut errs = CallbackErrors::default();
    errs.add(
        (p.on_user_message_callback().unwrap())(m.invocation(), text_content("user", "hello"))
            .await,
    );
    errs.add((p.before_run_callback().unwrap())(m.invocation()).await);
    errs.add((p.before_agent_callback().unwrap())(m.context()).await);
    errs.add(
        (p.before_model_callback().unwrap())(m.context(), llm_request)
            .await
            .1,
    );
    errs.add(after_model(&tp, &m, llm_response, None).await);
    errs.add(
        (p.on_model_error_callback().unwrap())(
            m.context(),
            LlmRequest {
                model: "gemini-test".into(),
                ..LlmRequest::default()
            },
            Arc::new(ModelError::Other("model unavailable".into())),
        )
        .await,
    );
    errs.add(
        (p.before_tool_callback().unwrap())(
            m.context(),
            Arc::clone(&lookup_tool),
            tool_args.clone(),
        )
        .await,
    );
    errs.add(
        (p.after_tool_callback().unwrap())(
            m.context(),
            Arc::clone(&lookup_tool),
            tool_args.clone(),
            json!({"answer": "sunny"}).as_object().unwrap().clone(),
            None,
        )
        .await,
    );
    errs.add(
        (p.on_tool_error_callback().unwrap())(
            m.context(),
            Arc::clone(&lookup_tool),
            tool_args.clone(),
            Arc::new(ToolError::Execution("lookup failed".into())),
        )
        .await,
    );
    errs.add((p.on_event_callback().unwrap())(m.invocation(), ev).await);
    errs.add((p.after_agent_callback().unwrap())(m.context()).await);
    errs.check();
    after_run(&tp, &m).await;

    let rows = tp
        .collect_rows(until_event_type(EVENT_INVOCATION_COMPLETED))
        .await;

    let want = vec![
        WantRow {
            event_type: EVENT_USER_MESSAGE_RECEIVED,
            content: r#"{"text_summary":"hello"}"#,
            parts: vec![text_part("hello", 0)],
            attrs: vec![("adk.author", r#""user""#)],
            ..WantRow::default()
        },
        WantRow {
            event_type: EVENT_INVOCATION_STARTING,
            ..WantRow::default()
        },
        WantRow {
            event_type: EVENT_AGENT_STARTING,
            ..WantRow::default()
        },
        WantRow {
            event_type: EVENT_LLM_REQUEST,
            content: r#"{"prompt":[{"role":"user","content":"hello"}],"system_prompt":"be brief"}"#,
            parts: vec![text_part("hello", 0), text_part("be brief", 0)],
            attrs: vec![
                ("model", r#""gemini-test""#),
                ("llm_config", r#"{"temperature":0.5}"#),
                ("labels", r#"{"env":"test"}"#),
            ],
            ..WantRow::default()
        },
        WantRow {
            event_type: EVENT_LLM_RESPONSE,
            content: r#"{"response":"text: 'hi there'","usage":{"prompt":10,"completion":5,"total":15}}"#,
            parts: vec![text_part("hi there", 0)],
            attrs: vec![
                ("model_version", r#""gemini-test-001""#),
                ("finish_reason", r#""STOP""#),
                ("partial", "false"),
                ("turn_complete", "true"),
                ("interrupted", "false"),
                (
                    "usage_metadata",
                    r#"{"prompt_token_count":10,"candidates_token_count":5,"total_token_count":15}"#,
                ),
                ("cache_type", r#""none""#),
            ],
            ..WantRow::default()
        },
        WantRow {
            event_type: EVENT_LLM_ERROR,
            status: "ERROR",
            err_msg: "model unavailable",
            attrs: vec![("model", r#""gemini-test""#)],
            ..WantRow::default()
        },
        WantRow {
            event_type: EVENT_TOOL_STARTING,
            content: r#"{"tool":"lookup","args":{"query":"weather","big":9007199254740993},"tool_origin":"UNKNOWN"}"#,
            attrs: vec![
                ("tool_description", r#""Looks things up""#),
                ("is_long_running", "true"),
                ("adk.function_call_id", r#""call-1""#),
            ],
            ..WantRow::default()
        },
        WantRow {
            event_type: EVENT_TOOL_COMPLETED,
            content: r#"{"tool":"lookup","result":{"answer":"sunny"},"tool_origin":"UNKNOWN"}"#,
            attrs: vec![("adk.function_call_id", r#""call-1""#)],
            ..WantRow::default()
        },
        WantRow {
            event_type: EVENT_TOOL_ERROR,
            status: "ERROR",
            err_msg: "lookup failed",
            content: r#"{"tool":"lookup","args":{"query":"weather","big":9007199254740993},"tool_origin":"UNKNOWN"}"#,
            attrs: vec![("adk.function_call_id", r#""call-1""#)],
            ..WantRow::default()
        },
        WantRow {
            event_type: EVENT_AGENT_RESPONSE,
            content: r#"{"response":"text: 'event text'"}"#,
            parts: vec![text_part("event text", 0)],
            attrs: vec![
                ("event_author", r#""test-agent""#),
                ("source_event_id", r#""event-1""#),
                ("source_event_author", r#""test-agent""#),
                ("source_event_branch", r#""root.test-agent""#),
                ("adk.source_event_id", r#""event-1""#),
                ("adk.branch", r#""root.test-agent""#),
                (
                    "adk.node",
                    r#"{"path":"wf@0/review@2","run_id":"2","parent_run_id":"0","output_for":["wf@0"]}"#,
                ),
                ("adk.isolation_scope", r#""wf@0/review@2""#),
                ("adk.scope", r#"{"id":"wf@0/review@2","kind":"node_run"}"#),
                ("adk.route", r#"["approve"]"#),
                ("adk.artifact_delta", r#"{"report.txt":3}"#),
                ("adk.escalate", "true"),
            ],
            ..WantRow::default()
        },
        WantRow {
            event_type: EVENT_AGENT_COMPLETED,
            ..WantRow::default()
        },
        WantRow {
            event_type: EVENT_INVOCATION_COMPLETED,
            ..WantRow::default()
        },
    ];

    let want_types: Vec<&str> = want.iter().map(|w| w.event_type).collect();
    assert_eq!(want_types, event_types(&rows), "event types mismatch");

    let mut seen_ids = std::collections::BTreeSet::new();
    for (row, w) in rows.iter().zip(&want) {
        let et = w.event_type;
        let event_id = str_of(row, "event_id").unwrap_or_default();
        assert!(
            EVENT_ID_PATTERN.is_match(&event_id),
            "{et}: event_id = {event_id:?}, want 32 lowercase hex digits"
        );
        assert!(
            seen_ids.insert(event_id.clone()),
            "{et}: event_id {event_id:?} is not unique"
        );

        match row.get("timestamp") {
            Some(RowValue::Timestamp(ts))
                if ts.timestamp_micros() >= start.timestamp_micros() && *ts <= Utc::now() => {}
            other => panic!("{et}: timestamp = {other:?}, want a time after {start}"),
        }

        let want_status = if w.status.is_empty() { "OK" } else { w.status };
        let mut want_columns = BTreeMap::from([
            ("event_type", RowValue::Str(et.into())),
            ("agent", RowValue::Str("test-agent".into())),
            ("session_id", RowValue::Str("sess-id".into())),
            ("invocation_id", RowValue::Str("inv-id".into())),
            ("user_id", RowValue::Str("user-id".into())),
            ("status", RowValue::Str(want_status.into())),
            ("is_truncated", RowValue::Bool(false)),
        ]);
        if !w.err_msg.is_empty() {
            want_columns.insert("error_message", RowValue::Str(w.err_msg.into()));
        }
        let mut got_columns = BTreeMap::new();
        for column in want_columns.keys() {
            if let Some(v) = row.get(*column) {
                got_columns.insert(*column, v.clone());
            }
        }
        if let Some(v) = row.get("error_message") {
            got_columns.insert("error_message", v.clone());
        }
        assert_eq!(want_columns, got_columns, "{et}: columns mismatch");

        let got_content = json_column(row, COLUMN_CONTENT);
        match (&got_content, w.content.is_empty()) {
            (Some(c), true) => panic!("{et}: content = {c}, want no content"),
            (None, false) => panic!("{et}: content column is missing"),
            (Some(c), false) => check_json_eq(&format!("{et}: content"), c, w.content),
            (None, true) => {}
        }
        assert_eq!(w.parts, parts_of(row), "{et}: content_parts mismatch");

        let attrs = json_object_column(row, COLUMN_ATTRIBUTES);
        let quoted_id = Value::String(event_id.clone()).to_string();
        let mut want_attrs: BTreeMap<&str, String> = [
            ("adk.schema_version", r#""1""#),
            ("adk.app_name", r#""test-app""#),
            ("adk.invocation_id", r#""inv-id""#),
            ("adk.session_id", r#""sess-id""#),
            ("adk.user_id", r#""user-id""#),
            ("adk.author", r#""test-agent""#),
            ("root_agent_name", r#""test-agent""#),
            ("custom_tags", r#"{"team":"search","labels":{"tier":"gold"}}"#),
            (
                "session_metadata",
                r#"{"session_id":"sess-id","app_name":"test-app","user_id":"user-id","state":{"user_pref":"dark","temp:scratch":"[REDACTED]","api_key":"[REDACTED]"}}"#,
            ),
        ]
        .into_iter()
        .map(|(k, v)| (k, v.to_owned()))
        .collect();
        want_attrs.insert("adk.event_id", quoted_id);
        for (k, v) in &w.attrs {
            want_attrs.insert(k, (*v).to_owned());
        }
        for (path, want_json) in &want_attrs {
            let keys: Vec<&str> = path.split('.').collect();
            let name = format!("{et}: {path}");
            let got =
                lookup(&attrs, &keys).unwrap_or_else(|| panic!("{name} is missing; got {attrs}"));
            check_json_eq(&name, got, want_json);
        }
        if let Some(v) = lookup(&attrs, &["adk", "adk_version"]) {
            assert!(
                !(v == "" || v == "(devel)" || v == "0.0.0"),
                "{et}: attributes.adk.adk_version = {v}, want either absent or a real version"
            );
        }
        let has_branch = lookup(&attrs, &["adk", "branch"]).is_some();
        assert_eq!(
            has_branch,
            et == EVENT_AGENT_RESPONSE,
            "{et}: attributes.adk.branch present"
        );
        let has_call_id = lookup(&attrs, &["adk", "function_call_id"]).is_some();
        assert_eq!(
            has_call_id,
            et.starts_with("TOOL_"),
            "{et}: attributes.adk.function_call_id present"
        );
    }

    // The checks above compare numbers as values; the column itself must keep every digit.
    let raw = str_of(
        &only_row_of_type(&rows, EVENT_TOOL_STARTING),
        COLUMN_CONTENT,
    )
    .unwrap_or_default();
    assert!(
        raw.contains(r#""big":9007199254740993"#),
        "TOOL_STARTING content {raw} lost the precision of a 64-bit integer"
    );
}

// parity: adapted: Go's span context in mCtx.ctx → the callback bodies called with CallbackCtx::with_span (gadk reads the ambient span from the tracing subscriber); the cases outside a span go through the plugin's callbacks
#[tokio::test]
async fn test_attributes_optional_sections() {
    const TRACE_HEX: &str = "4bf92f3577b34da6a3ce929d0e0e4736";
    const SPAN_HEX: &str = "00f067aa0ba902b7";
    let sc = span_ids(TRACE_HEX, SPAN_HEX);
    let otel = format!(r#"{{"span_id":"{SPAN_HEX}","trace_id":"{TRACE_HEX}"}}"#);

    type Mutate = fn(&mut Config);
    // want maps an attributes path to its JSON value; "" means that the path must be absent.
    type Want<'a> = Vec<(&'a str, String)>;
    let tests: Vec<(&str, Mutate, bool, Want)> = vec![
        (
            "defaults",
            |_| {},
            true,
            vec![
                (
                    "session_metadata",
                    r#"{"session_id":"sess-id","app_name":"test-app","user_id":"user-id"}"#.into(),
                ),
                ("adk.session_id", r#""sess-id""#.into()),
                ("adk.user_id", r#""user-id""#.into()),
                ("custom_tags", String::new()),
                ("otel", String::new()),
            ],
        ),
        (
            "DisableSessionMetadata",
            |c| c.disable_session_metadata = true,
            false,
            vec![
                ("session_metadata", String::new()),
                ("adk.session_id", String::new()),
                ("adk.user_id", String::new()),
                ("adk.invocation_id", r#""inv-id""#.into()),
            ],
        ),
        (
            "CustomTags",
            |c| {
                c.custom_tags = json!({"team": "search"}).as_object().unwrap().clone();
            },
            false,
            vec![("custom_tags", r#"{"team":"search"}"#.into())],
        ),
        (
            "EnableOTelCorrelation inside a span",
            |c| c.enable_otel_correlation = true,
            true,
            vec![("otel", otel.clone())],
        ),
        (
            "EnableOTelCorrelation outside a span",
            |c| c.enable_otel_correlation = true,
            false,
            vec![("otel", String::new())],
        ),
    ];
    for (name, mutate, in_span, want) in tests {
        let msg = text_content("user", "hello");
        let rows = if in_span {
            let mut sp = SpanPlugin::new(mutate).await;
            sp.tp.ctx.app_name = "test-app".into();
            sp.user_message_then_after_run(Some(sc), &msg).await;
            sp.tp
                .collect_rows(until_event_type(EVENT_INVOCATION_COMPLETED))
                .await
        } else {
            let tp = new_test_plugin_with_config(mutate).await;
            let mut m = tp.ctx.clone();
            m.app_name = "test-app".into();
            let m = Mc::new(&m);
            (tp.plugin.on_user_message_callback().unwrap())(m.invocation(), msg)
                .await
                .unwrap_or_else(|e| panic!("{name}: OnUserMessageCallback error: {e}"));
            after_run(&tp, &m).await;
            tp.collect_rows(until_event_type(EVENT_INVOCATION_COMPLETED))
                .await
        };
        let row = only_row_of_type(&rows, EVENT_USER_MESSAGE_RECEIVED);
        // The row belongs to the invocation span. Inside an ambient span, the invocation
        // continues the ambient trace under the ambient span; outside one, it starts a trace
        // of its own.
        let got_trace = str_of(&row, "trace_id").unwrap_or_default();
        let got_span = str_of(&row, "span_id").unwrap_or_default();
        let got_parent = str_of(&row, "parent_span_id");
        if in_span {
            assert!(
                got_trace == TRACE_HEX && got_parent.as_deref() == Some(SPAN_HEX),
                "{name}: trace_id, parent_span_id = {got_trace:?}, {got_parent:?}, want {TRACE_HEX:?}, {SPAN_HEX:?}"
            );
        } else {
            assert!(
                EVENT_ID_PATTERN.is_match(&got_trace) && got_parent.is_none(),
                "{name}: trace_id, parent_span_id = {got_trace:?}, {got_parent:?}, want 32 hex digits and no parent"
            );
        }
        assert!(
            SPAN_ID_PATTERN.is_match(&got_span) && got_span != SPAN_HEX,
            "{name}: span_id = {got_span:?}, want 16 hex digits of a span of the invocation"
        );
        let attrs = json_object_column(&row, COLUMN_ATTRIBUTES);
        for (path, want_json) in &want {
            let keys: Vec<&str> = path.split('.').collect();
            if want_json.is_empty() {
                if let Some(v) = lookup(&attrs, &keys) {
                    panic!("{name}: attributes.{path} = {v}, want it absent");
                }
                continue;
            }
            check_json_at(&attrs, want_json, &keys);
        }
    }
}

#[tokio::test]
async fn test_sensitive_key_redaction() {
    type Mutate = fn(&mut Config);
    let tests: [(&str, Mutate, &str, &str); 2] = [
        (
            "redacted by default",
            |_| {},
            REDACTED_VALUE,
            "https://example.com/i.png?api_key=%5BREDACTED%5D#access_token=%5BREDACTED%5D",
        ),
        (
            "DisableSensitiveKeyRedaction keeps values",
            |c| c.disable_sensitive_key_redaction = true,
            "sk-live",
            "https://example.com/i.png?api_key=sk-live#access_token=sk-live",
        ),
    ];
    for (name, mutate, want, want_uri) in tests {
        let tp = new_test_plugin_with_config(mutate).await;
        let mut m = tp.ctx.clone();
        m.state = json!({"temp:token": "sk-live", "theme": "dark"})
            .as_object()
            .unwrap()
            .clone();
        let m = Mc::new(&m);
        let args = json!({
            "api_key": "sk-live",
            "query": "weather",
            "headers": {"Authorization": "sk-live", "Accept": "json"},
            "items": [{"client-secret": "sk-live"}],
            "json": r#"{"api_key":"sk-live"}"#,
        })
        .as_object()
        .unwrap()
        .clone();
        let p = &tp.plugin;
        let mut errs = CallbackErrors::default();
        errs.add((p.before_tool_callback().unwrap())(m.context(), fake_tool("lookup"), args).await);
        let uri_part = Part {
            file_data: Some(FileData {
                mime_type: "image/png".into(),
                file_uri: "https://example.com/i.png?api_key=sk-live#access_token=sk-live".into(),
                ..Default::default()
            }),
            ..Part::default()
        };
        errs.add(
            (p.on_user_message_callback().unwrap())(
                m.invocation(),
                Content {
                    role: "user".into(),
                    parts: vec![uri_part],
                },
            )
            .await,
        );
        errs.add(
            (p.on_model_error_callback().unwrap())(
                m.context(),
                LlmRequest::default(),
                Arc::new(ModelError::Other(
                    r#"auth failed: api_key=sk-live Bearer sk-live https://example.com/v1?key=sk-live {"client_secret":"sk-live"}"#.into(),
                )),
            )
            .await,
        );
        errs.check();
        after_run(&tp, &m).await;

        let rows = tp.collect_rows(until_invocation_end).await;
        let row = only_row_of_type(&rows, EVENT_TOOL_STARTING);
        let inner = Value::String(format!(r#"{{"api_key":{}}}"#, Value::String(want.into())));
        check_json_at(
            &content_of(&row),
            &format!(
                r#"{{"api_key":{w},"query":"weather","headers":{{"Authorization":{w},"Accept":"json"}},"items":[{{"client-secret":{w}}}],"json":{inner}}}"#,
                w = Value::String(want.into())
            ),
            &["args"],
        );
        check_json_at(
            &json_object_column(&row, COLUMN_ATTRIBUTES),
            &format!(
                r#"{{"temp:token":{},"theme":"dark"}}"#,
                Value::String(want.into())
            ),
            &["session_metadata", "state"],
        );

        let user_row = only_row_of_type(&rows, EVENT_USER_MESSAGE_RECEIVED);
        let parts = parts_of(&user_row);
        assert!(
            parts.len() == 1 && parts[0].get("uri") == Some(&RowValue::Str(want_uri.into())),
            "{name}: content_parts uri = {parts:?}, want {want_uri:?}"
        );

        let err_row = only_row_of_type(&rows, EVENT_LLM_ERROR);
        let err_msg = str_of(&err_row, "error_message").unwrap_or_default();
        if want == REDACTED_VALUE {
            assert!(
                !err_msg.contains("sk-live"),
                "{name}: error_message {err_msg:?} leaked credential sk-live"
            );
        } else {
            assert!(
                err_msg.contains("sk-live"),
                "{name}: error_message {err_msg:?} did not preserve sk-live when redaction was disabled"
            );
        }
    }
}

// parity: adapted: Go's result struct with fields APIKey / ClientSecret / Region → the JSON object it marshals to (gadk tool results are JSON maps)
#[tokio::test]
async fn test_sensitive_key_redaction_go_names_and_state_prefixes() {
    const SECRET: &str = "SECRET123";
    let tp = setup_test_plugin().await;
    let mut m = tp.ctx.clone();
    m.state = json!({"user:api_key": SECRET, "app:clientSecret": SECRET, "user:theme": "dark"})
        .as_object()
        .unwrap()
        .clone();
    let m = Mc::new(&m);
    let args = json!({"accessToken": SECRET, "x-goog-api-key": SECRET, "tokenCount": 3})
        .as_object()
        .unwrap()
        .clone();
    let result = json!({"config": {"APIKey": SECRET, "ClientSecret": SECRET, "Region": "us"}})
        .as_object()
        .unwrap()
        .clone();
    let p = &tp.plugin;
    let mut errs = CallbackErrors::default();
    errs.add(
        (p.before_tool_callback().unwrap())(m.context(), fake_tool("lookup"), args.clone()).await,
    );
    errs.add(
        (p.after_tool_callback().unwrap())(m.context(), fake_tool("lookup"), args, result, None)
            .await,
    );
    errs.check();
    after_run(&tp, &m).await;

    let rows = tp
        .collect_rows(until_event_type(EVENT_INVOCATION_COMPLETED))
        .await;
    let start_row = only_row_of_type(&rows, EVENT_TOOL_STARTING);
    check_json_at(
        &content_of(&start_row),
        r#"{"accessToken":"[REDACTED]","x-goog-api-key":"[REDACTED]","tokenCount":3}"#,
        &["args"],
    );
    check_json_at(
        &json_object_column(&start_row, COLUMN_ATTRIBUTES),
        r#"{"user:api_key":"[REDACTED]","app:clientSecret":"[REDACTED]","user:theme":"dark"}"#,
        &["session_metadata", "state"],
    );
    check_json_at(
        &content_of(&only_row_of_type(&rows, EVENT_TOOL_COMPLETED)),
        r#"{"config":{"APIKey":"[REDACTED]","ClientSecret":"[REDACTED]","Region":"us"}}"#,
        &["result"],
    );
    for row in &rows {
        let s = format!("{row:?}");
        assert!(
            !s.contains(SECRET),
            "{:?} row leaked {SECRET}: {s}",
            row.get("event_type")
        );
    }
}

#[tokio::test]
async fn test_credentials_envelope_is_redacted_whole() {
    const SECRET: &str = "SECRET123";
    let envelope = || {
        json!({
            "AccessKeyId": "EXAMPLEKEYID",
            "SecretAccessKey": SECRET,
            "SessionToken": SECRET,
            "Expiration": "2026-10-14T00:00:00Z",
        })
    };
    let tp = setup_test_plugin().await;
    let m = Mc::new(&tp.ctx.clone());
    let args = json!({"role": "reader", "credential": envelope()})
        .as_object()
        .unwrap()
        .clone();
    let result = json!({
        "Credentials": envelope(),
        "keys": {"AccessKeyId": "EXAMPLEKEYID", "secret_access_key": SECRET, "session-token": SECRET, "aws_secret_access_key": SECRET, "AWS_SESSION_TOKEN": SECRET},
    })
    .as_object()
    .unwrap()
    .clone();
    let p = &tp.plugin;
    let mut errs = CallbackErrors::default();
    errs.add(
        (p.before_tool_callback().unwrap())(m.context(), fake_tool("assume_role"), args.clone())
            .await,
    );
    errs.add(
        (p.after_tool_callback().unwrap())(
            m.context(),
            fake_tool("assume_role"),
            args,
            result,
            None,
        )
        .await,
    );
    errs.check();
    after_run(&tp, &m).await;

    let rows = tp
        .collect_rows(until_event_type(EVENT_INVOCATION_COMPLETED))
        .await;
    check_json_at(
        &content_of(&only_row_of_type(&rows, EVENT_TOOL_STARTING)),
        r#"{"role":"reader","credential":"[REDACTED]"}"#,
        &["args"],
    );
    check_json_at(
        &content_of(&only_row_of_type(&rows, EVENT_TOOL_COMPLETED)),
        r#"{"Credentials":"[REDACTED]","keys":{"AccessKeyId":"EXAMPLEKEYID","secret_access_key":"[REDACTED]","session-token":"[REDACTED]","aws_secret_access_key":"[REDACTED]","AWS_SESSION_TOKEN":"[REDACTED]"}}"#,
        &["result"],
    );
    for row in &rows {
        let s = format!("{row:?}");
        assert!(
            !s.contains(SECRET),
            "{:?} row leaked {SECRET}: {s}",
            row.get("event_type")
        );
    }
}

// parity: adapted: gadk tool args are a serde_json Map, which cannot hold Go's panicking marshaler, cyclic map or NaN (the formatter's handling of them is ported in 7b-1's json_formatter tests); the 64-bit integer keeps its digits and the row is written, untruncated
#[tokio::test]
async fn test_tool_args_unloggable_values_keep_the_row() {
    let tp = setup_test_plugin().await;
    let m = Mc::new(&tp.ctx.clone());
    let args = json!({"big": 9_007_199_254_740_993_i64, "name": "loop"})
        .as_object()
        .unwrap()
        .clone();
    (tp.plugin.before_tool_callback().unwrap())(m.context(), fake_tool("lookup"), args)
        .await
        .unwrap_or_else(|e| panic!("BeforeToolCallback error: {e}"));
    after_run(&tp, &m).await;

    let row = only_row_of_type(
        &tp.collect_rows(until_event_type(EVENT_INVOCATION_COMPLETED))
            .await,
        EVENT_TOOL_STARTING,
    );
    check_json_at(
        &content_of(&row),
        r#"{"big":9007199254740993,"name":"loop"}"#,
        &["args"],
    );
    let raw = str_of(&row, COLUMN_CONTENT).unwrap_or_default();
    assert!(
        raw.contains(r#""big":9007199254740993"#),
        "content {raw} lost the precision of a 64-bit integer"
    );
    assert_eq!(
        row.get("is_truncated"),
        Some(&RowValue::Bool(false)),
        "is_truncated with every value loggable"
    );
}

/// One case of [`test_content_formatter`].
struct FormatterCase {
    name: &'static str,
    formatter: ContentFormatter,
    debug: bool,
    deny_content: bool,
    want_content: &'static str,
    want_fmt_failed: bool,
    want_err_msg: &'static str,
    want_parts: bool,
    want_log: String,
    want_no_log: &'static str,
}

// parity: adapted: Go's formatter error type `*errors.errorString` → `error` in error_message and the log (Rust has no dynamic type name; 7b-1's ledgered divergence)
#[tokio::test]
async fn test_content_formatter() {
    const SECRET: &str = "formatter-secret-detail";
    let failing: ContentFormatter = Arc::new(|_, _| Err(SECRET.into()));
    let tests = vec![
        FormatterCase {
            name: "replaces the content",
            formatter: Arc::new(|_, event_type| Ok(json!({"event": event_type, "redacted": true}))),
            debug: false,
            deny_content: false,
            want_content: r#"{"event":"USER_MESSAGE_RECEIVED","redacted":true}"#,
            want_fmt_failed: false,
            want_err_msg: "",
            want_parts: false,
            want_log: String::new(),
            want_no_log: "",
        },
        FormatterCase {
            name: "passes the content through",
            formatter: Arc::new(|payload, _| Ok(payload.clone())),
            debug: false,
            deny_content: false,
            want_content: r#"{"text_summary":"hello"}"#,
            want_fmt_failed: false,
            want_err_msg: "",
            want_parts: true,
            want_log: String::new(),
            want_no_log: "",
        },
        FormatterCase {
            name: "error fails closed",
            formatter: Arc::clone(&failing),
            debug: false,
            deny_content: false,
            want_content: r#""[FORMATTER_FAILED]""#,
            want_fmt_failed: true,
            want_err_msg: "content_formatter raised error",
            want_parts: false,
            want_log: "ContentFormatter failed for event USER_MESSAGE_RECEIVED".into(),
            want_no_log: SECRET,
        },
        FormatterCase {
            name: "debug mode logs error type and length without raw error text",
            formatter: Arc::clone(&failing),
            debug: true,
            deny_content: false,
            want_content: r#""[FORMATTER_FAILED]""#,
            want_fmt_failed: true,
            want_err_msg: "content_formatter raised error",
            want_parts: false,
            want_log: format!("err_len={}", SECRET.len()),
            want_no_log: SECRET,
        },
        FormatterCase {
            name: "panic fails closed",
            formatter: Arc::new(|_, _| panic!("{SECRET}")),
            debug: false,
            deny_content: false,
            want_content: r#""[FORMATTER_FAILED]""#,
            want_fmt_failed: true,
            want_err_msg: "content_formatter panicked",
            want_parts: false,
            want_log: "ContentFormatter panicked for event USER_MESSAGE_RECEIVED".into(),
            want_no_log: SECRET,
        },
        FormatterCase {
            name: "error with content denied sets formatter_failed with diagnostic error_message",
            formatter: Arc::clone(&failing),
            debug: false,
            deny_content: true,
            want_content: "",
            want_fmt_failed: true,
            want_err_msg: "content_formatter raised error",
            want_parts: false,
            want_log: "ContentFormatter failed for event USER_MESSAGE_RECEIVED".into(),
            want_no_log: SECRET,
        },
    ];
    for tt in tests {
        let name = tt.name;
        let logger = Arc::new(RecordingLogger::default());
        let tp = new_test_plugin_with_config(|c| {
            c.content_formatter = Some(Arc::clone(&tt.formatter));
            c.debug_content_formatter_errors = tt.debug;
            c.logger = Some(logger.clone());
            if tt.deny_content {
                c.payload_column_denylist = vec![COLUMN_CONTENT.into()];
            }
        })
        .await;
        let m = Mc::new(&tp.ctx.clone());
        let got_msg = (tp.plugin.on_user_message_callback().unwrap())(
            m.invocation(),
            text_content("user", "hello"),
        )
        .await
        .unwrap_or_else(|e| panic!("{name}: OnUserMessageCallback error: {e}"));
        assert!(
            got_msg.is_none(),
            "{name}: OnUserMessageCallback returned {got_msg:?}, want nil so subsequent plugins run"
        );
        after_run(&tp, &m).await;

        let rows = tp
            .collect_rows(until_event_type(EVENT_INVOCATION_COMPLETED))
            .await;
        assert!(
            !only_row_of_type(&rows, EVENT_INVOCATION_COMPLETED).contains_key(COLUMN_CONTENT),
            "{name}: INVOCATION_COMPLETED row has content; the formatter must not run for events without content"
        );
        let row = only_row_of_type(&rows, EVENT_USER_MESSAGE_RECEIVED);
        if tt.want_content.is_empty() {
            assert!(
                !row.contains_key(COLUMN_CONTENT),
                "{name}: content = {:?}, want absent when content column is denied",
                row.get(COLUMN_CONTENT)
            );
        } else {
            let got = json_column(&row, COLUMN_CONTENT).unwrap_or(Value::Null);
            check_json_eq(&format!("{name}: content"), &got, tt.want_content);
        }
        assert_eq!(
            str_of(&row, "status").as_deref(),
            Some("OK"),
            "{name}: status"
        );
        if tt.want_err_msg.is_empty() {
            assert!(
                !row.contains_key("error_message"),
                "{name}: error_message = {:?}, want absent",
                row.get("error_message")
            );
        } else {
            assert_eq!(
                str_of(&row, "error_message").as_deref(),
                Some(tt.want_err_msg),
                "{name}: error_message"
            );
        }
        assert_eq!(
            row.get("is_truncated"),
            Some(&RowValue::Bool(false)),
            "{name}: is_truncated"
        );
        assert_eq!(
            row.contains_key(COLUMN_CONTENT_PARTS),
            tt.want_parts,
            "{name}: content_parts present"
        );
        let attrs = json_object_column(&row, COLUMN_ATTRIBUTES);
        let got_fmt_failed = lookup(&attrs, &["formatter_failed"]);
        assert!(
            got_fmt_failed.is_some() == tt.want_fmt_failed
                && (!tt.want_fmt_failed || got_fmt_failed == Some(&Value::Bool(true))),
            "{name}: attributes.formatter_failed = {got_fmt_failed:?}, want present = {}",
            tt.want_fmt_failed
        );
        if !tt.want_log.is_empty() {
            assert!(
                logger.contains(&tt.want_log),
                "{name}: log does not contain {:?}:\n{logger}",
                tt.want_log
            );
        }
        if !tt.want_no_log.is_empty() {
            assert!(
                !logger.contains(tt.want_no_log),
                "{name}: log contains {:?}:\n{logger}",
                tt.want_no_log
            );
        }
    }
}

// parity: adapted: Go's formatter error type `*errors.errorString` → `error` (7b-1's ledgered divergence)
#[tokio::test]
async fn test_content_formatter_preserves_existing_error_message() {
    let tp = new_test_plugin_with_config(|c| {
        c.content_formatter = Some(Arc::new(|_, _| Err("formatter secret".into())));
    })
    .await;
    let m = Mc::new(&tp.ctx.clone());
    after_model(
        &tp,
        &m,
        LlmResponse {
            content: Some(text_content("model", "partial")),
            ..LlmResponse::default()
        },
        Some(ModelError::Other("model failed".into())),
    )
    .await
    .unwrap_or_else(|e| panic!("AfterModelCallback error: {e}"));
    after_run(&tp, &m).await;

    let row = only_row_of_type(
        &tp.collect_rows(until_invocation_end).await,
        EVENT_LLM_RESPONSE,
    );
    assert_eq!(str_of(&row, "status").as_deref(), Some("ERROR"), "status");
    const WANT_ERR: &str = "model failed; content_formatter raised error";
    assert_eq!(
        str_of(&row, "error_message").as_deref(),
        Some(WANT_ERR),
        "error_message"
    );
    check_json_at(
        &json_object_column(&row, COLUMN_ATTRIBUTES),
        "true",
        &["formatter_failed"],
    );
}

#[test]
fn test_should_log_event() {
    let set = |items: &[&str]| items.iter().map(|s| (*s).to_owned()).collect();
    type Case<'a> = (&'a str, &'a [&'a str], &'a [&'a str], &'a str, bool);
    let tests: [Case; 6] = [
        ("no filters", &[], &[], EVENT_LLM_REQUEST, true),
        (
            "allowlisted",
            &[EVENT_LLM_REQUEST],
            &[],
            EVENT_LLM_REQUEST,
            true,
        ),
        (
            "not allowlisted",
            &[EVENT_TOOL_STARTING],
            &[],
            EVENT_LLM_REQUEST,
            false,
        ),
        (
            "denylisted",
            &[],
            &[EVENT_LLM_REQUEST],
            EVENT_LLM_REQUEST,
            false,
        ),
        (
            "denylist overrides allowlist",
            &[EVENT_LLM_REQUEST],
            &[EVENT_LLM_REQUEST],
            EVENT_LLM_REQUEST,
            false,
        ),
        (
            "denylist leaves other events",
            &[],
            &[EVENT_LLM_REQUEST],
            EVENT_TOOL_STARTING,
            true,
        ),
    ];
    for (name, allow, deny, event_type, want) in tests {
        let p = AnalyticsPlugin {
            allowlist: set(allow),
            denylist: set(deny),
            ..AnalyticsPlugin::default()
        };
        assert_eq!(
            p.should_log_event(event_type),
            want,
            "{name}: shouldLogEvent({event_type:?})"
        );
    }
}

#[tokio::test]
async fn test_event_filtering_denylist_overrides_allowlist() {
    let logger = Arc::new(RecordingLogger::default());
    let tp = new_test_plugin_with_config(|c| {
        c.logger = Some(logger.clone());
        c.event_allowlist = vec![
            EVENT_LLM_REQUEST.into(),
            EVENT_TOOL_STARTING.into(),
            "TOOL_STARTED".into(),
        ];
        c.event_denylist = vec![EVENT_LLM_REQUEST.into()];
    })
    .await;
    let m = Mc::new(&tp.ctx.clone());
    let p = &tp.plugin;
    let mut errs = CallbackErrors::default();
    errs.add(
        (p.before_model_callback().unwrap())(
            m.context(),
            LlmRequest {
                model: "gemini-test".into(),
                ..LlmRequest::default()
            },
        )
        .await
        .1,
    );
    errs.add(
        (p.before_tool_callback().unwrap())(m.context(), fake_tool("lookup"), Map::new()).await,
    );
    errs.check();
    after_run(&tp, &m).await;

    // Rows arrive in order, so a logged LLM_REQUEST row would arrive first.
    let rows = tp.collect_rows(until_event_type(EVENT_TOOL_STARTING)).await;
    assert_eq!(
        vec![EVENT_TOOL_STARTING],
        event_types(&rows),
        "event types mismatch"
    );
    let want = r#"unknown event_type "TOOL_STARTED""#;
    assert!(
        logger.contains(want),
        "log does not contain {want:?}:\n{logger}"
    );
    assert!(
        !logger.contains(&format!(r#"unknown event_type "{EVENT_LLM_REQUEST}"#)),
        "log warns about a known event type:\n{logger}"
    );
}

// parity: adapted: Go's negative BatchFlushIntv / ShutdownTimeout cases do not exist (a Rust Duration cannot be negative; 7b-1's ledgered divergence) and GOOGLE_APPLICATION_CREDENTIALS is not pointed at a missing file (setting the environment is unsafe in edition 2024; validation runs before any client either way)
#[tokio::test]
async fn test_new_plugin_rejects_invalid_config() {
    type Mutate = fn(&mut Config);
    let tests: Vec<(&str, Mutate, &str)> = vec![
        (
            "unknown payload column",
            |c| c.payload_column_denylist = vec!["error_message".into()],
            r#"unsupported PayloadColumnDenylist entry "error_message""#,
        ),
        (
            "custom metadata without the attributes column",
            |c| {
                c.payload_column_denylist = vec![COLUMN_ATTRIBUTES.into()];
                c.custom_metadata_allowlist = vec!["a2a:*".into()];
            },
            "CustomMetadataAllowlist cannot be used",
        ),
        (
            "MaxContentLen less than -1",
            |c| c.max_content_len = -2,
            "MaxContentLen must be -1 or >= 1",
        ),
        (
            "negative BatchSize",
            |c| c.batch_size = -1,
            "BatchSize and QueueMaxSize must be >= 1",
        ),
        (
            "negative QueueMaxSize",
            |c| c.queue_max_size = -1,
            "BatchSize and QueueMaxSize must be >= 1",
        ),
        (
            "negative RetryConfig.MaxRetries",
            |c| c.retry_config.max_retries = -1,
            "invalid RetryConfig",
        ),
        (
            "Multiplier less than 1",
            |c| c.retry_config.multiplier = 0.5,
            "invalid RetryConfig",
        ),
        (
            "MaxDelay less than InitialDelay",
            |c| {
                c.retry_config.initial_delay = Duration::from_secs(10);
                c.retry_config.max_delay = Duration::from_secs(5);
            },
            "invalid RetryConfig",
        ),
        (
            "invalid ViewPrefix with SQL injection characters",
            |c| c.view_prefix = "v` OR 1=1 --".into(),
            "invalid ViewPrefix",
        ),
    ];
    for (name, mutate, want_err) in tests {
        let ctx = Context::background();
        let mut config = Config {
            project_id: "test-project".into(),
            ..Config::default()
        };
        mutate(&mut config);
        // Validation happens before any client is created or used.
        match new_bigquery_agent_analytics_plugin_with_config(&ctx, config.clone()).await {
            Err(e) if e.to_string().contains(want_err) => {}
            other => panic!(
                "{name}: NewBigQueryAgentAnalyticsPluginWithConfig() error = {:?}, want an error containing {want_err:?}",
                other.err().map(|e| e.to_string())
            ),
        }
        match new_bigquery_agent_analytics_plugin_with_clients(&ctx, config, None, None).await {
            Err(e) if e.to_string().contains(want_err) => {}
            other => panic!(
                "{name}: NewBigQueryAgentAnalyticsPluginWithClients() error = {:?}, want an error containing {want_err:?}",
                other.err().map(|e| e.to_string())
            ),
        }
    }
}

#[tokio::test]
async fn test_payload_columns() {
    type Mutate = fn(&mut Config);
    let tests: Vec<(&str, Mutate, bool, bool, bool)> = vec![
        ("default", |_| {}, true, true, true),
        (
            "LogMultiModalContent cleared",
            |c| c.log_multi_modal_content = false,
            true,
            false,
            true,
        ),
        (
            "DisableMultiModalContent",
            |c| c.disable_multi_modal_content = true,
            true,
            false,
            true,
        ),
        (
            "content_parts denied",
            |c| c.payload_column_denylist = vec![COLUMN_CONTENT_PARTS.into()],
            true,
            false,
            true,
        ),
        (
            "content denied",
            |c| c.payload_column_denylist = vec![COLUMN_CONTENT.into()],
            false,
            true,
            true,
        ),
        (
            "content and content_parts denied",
            |c| {
                c.payload_column_denylist =
                    vec![COLUMN_CONTENT.into(), COLUMN_CONTENT_PARTS.into()];
            },
            false,
            false,
            true,
        ),
        (
            "attributes denied",
            |c| c.payload_column_denylist = vec![COLUMN_ATTRIBUTES.into()],
            true,
            true,
            false,
        ),
        (
            "latency_ms denied",
            |c| c.payload_column_denylist = vec![COLUMN_LATENCY_MS.into()],
            true,
            true,
            true,
        ),
    ];
    for (name, mutate, want_content, want_parts, want_attrs) in tests {
        let formatter_calls = Arc::new(AtomicUsize::new(0));
        let calls = Arc::clone(&formatter_calls);
        let tp = new_test_plugin_with_config(move |c| {
            c.content_formatter = Some(Arc::new(move |payload, _| {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(payload.clone())
            }));
            mutate(c);
        })
        .await;
        let m = Mc::new(&tp.ctx.clone());
        (tp.plugin.on_user_message_callback().unwrap())(
            m.invocation(),
            text_content("user", "hello"),
        )
        .await
        .unwrap_or_else(|e| panic!("{name}: OnUserMessageCallback error: {e}"));
        after_run(&tp, &m).await;

        let row = only_row_of_type(
            &tp.collect_rows(until_event_type(EVENT_INVOCATION_COMPLETED))
                .await,
            EVENT_USER_MESSAGE_RECEIVED,
        );
        for (column, want) in [
            (COLUMN_CONTENT, want_content),
            (COLUMN_CONTENT_PARTS, want_parts),
            (COLUMN_ATTRIBUTES, want_attrs),
        ] {
            assert_eq!(row.contains_key(column), want, "{name}: {column} present");
        }
        // The formatter runs only when a content column is written.
        assert_eq!(
            formatter_calls.load(Ordering::SeqCst) > 0,
            want_content || want_parts,
            "{name}: ContentFormatter called"
        );
    }
}

#[test]
fn test_custom_metadata_filter() {
    let md = json!({"trace_tag": "t", "a2a:task_id": "task-1", "a2a:context_id": "ctx-1", "private": "p"});
    let md = md.as_object().unwrap();
    let tests: [(&str, &[&str], Option<Value>); 5] = [
        ("empty allowlist captures nothing", &[], None),
        (
            "exact entry",
            &["trace_tag"],
            Some(json!({"trace_tag": "t"})),
        ),
        (
            "prefix entry",
            &["a2a:*"],
            Some(json!({"a2a:task_id": "task-1", "a2a:context_id": "ctx-1"})),
        ),
        ("entries without * match exactly", &["a2a:"], None),
        ("no match", &["other"], None),
    ];
    for (name, allowlist, want) in tests {
        let allowlist: Vec<String> = allowlist.iter().map(|s| (*s).to_owned()).collect();
        let got = CustomMetadataFilter::new(&allowlist)
            .apply(md)
            .map(Value::Object);
        assert_eq!(want, got, "{name}: apply() mismatch");
    }
}

#[tokio::test]
async fn test_custom_metadata_allowlist() {
    let tests: [(&str, &[&str], &str); 2] = [
        (
            "allowlisted keys are written",
            &["a2a:*"],
            r#"{"a2a:task_id":"task-1"}"#,
        ),
        ("nothing is written by default", &[], ""),
    ];
    for (name, allowlist, want) in tests {
        let tp = new_test_plugin_with_config(|c| {
            c.custom_metadata_allowlist = allowlist.iter().map(|s| (*s).to_owned()).collect();
        })
        .await;
        let m = Mc::new(&tp.ctx.clone());
        let md = json!({"a2a:task_id": "task-1", "private": "p"})
            .as_object()
            .unwrap()
            .clone();
        let p = &tp.plugin;
        let mut errs = CallbackErrors::default();
        let got_ev = (p.on_event_callback().unwrap())(
            m.invocation(),
            Event {
                author: "test-agent".into(),
                response: LlmResponse {
                    content: Some(text_content("model", "hi")),
                    custom_metadata: md.clone(),
                    ..LlmResponse::default()
                },
                ..Event::default()
            },
        )
        .await;
        if let Ok(Some(ev)) = &got_ev {
            panic!("{name}: OnEventCallback returned {ev:?}, want nil so subsequent plugins run");
        }
        errs.add(got_ev);
        errs.add(
            after_model(
                &tp,
                &m,
                LlmResponse {
                    custom_metadata: md,
                    ..LlmResponse::default()
                },
                None,
            )
            .await,
        );
        errs.check();
        after_run(&tp, &m).await;

        let rows = tp
            .collect_rows(until_event_type(EVENT_INVOCATION_COMPLETED))
            .await;
        for event_type in [EVENT_AGENT_RESPONSE, EVENT_LLM_RESPONSE] {
            let attrs = json_object_column(&only_row_of_type(&rows, event_type), COLUMN_ATTRIBUTES);
            let got = lookup(&attrs, &["custom_metadata"]);
            if want.is_empty() {
                assert!(
                    got.is_none(),
                    "{name}: {event_type}: custom_metadata = {got:?}, want none"
                );
            } else {
                check_json_at(&attrs, want, &["custom_metadata"]);
            }
        }
    }
}

#[tokio::test]
async fn test_flush_on_run_end() {
    for (name, disable, want_flush_at_end) in [
        ("flushes when the run ends", false, true),
        ("DisableFlushOnRunEnd defers the write", true, false),
    ] {
        let tp = new_test_plugin_with_config(|c| {
            // Only the run-end flush and Close write rows.
            c.batch_flush_intv = Duration::from_secs(3600);
            c.disable_flush_on_run_end = disable;
        })
        .await;
        let m = Mc::new(&tp.ctx.clone());
        (tp.plugin.on_user_message_callback().unwrap())(
            m.invocation(),
            text_content("user", "hello"),
        )
        .await
        .unwrap_or_else(|e| panic!("{name}: OnUserMessageCallback error: {e}"));
        after_run(&tp, &m).await;
        if !want_flush_at_end {
            // Go expectNoRequest(t, requests, 300*time.Millisecond).
            tokio::time::sleep(Duration::from_millis(300)).await;
            let early = tp.server.append_requests();
            assert!(
                early.is_empty(),
                "{name}: unexpected AppendRowsRequest with event types {:?}",
                event_types(&tp.rows())
            );
            tp.close()
                .await
                .unwrap_or_else(|e| panic!("{name}: Close() error: {e}"));
        }
        let rows = tp
            .collect_rows(until_event_type(EVENT_INVOCATION_COMPLETED))
            .await;
        assert_eq!(
            vec![EVENT_USER_MESSAGE_RECEIVED, EVENT_INVOCATION_COMPLETED],
            event_types(&rows),
            "{name}: event types mismatch"
        );
    }
}

#[tokio::test]
async fn test_final_response_tool_names() {
    let tp = new_test_plugin_with_config(|c| {
        c.final_response_tool_names = vec!["final_answer".into()];
    })
    .await;
    let mut m = tp.ctx.clone();
    m.function_call_id = "call-7".into();
    let m = Mc::new(&m);
    let final_tool = fake_tool("final_answer");
    let answer = json!({"answer": "42"}).as_object().unwrap().clone();
    let p = &tp.plugin;
    let after_tool = p.after_tool_callback().unwrap();
    let mut errs = CallbackErrors::default();
    errs.add(
        after_tool(
            m.context(),
            Arc::clone(&final_tool),
            answer.clone(),
            Map::new(),
            None,
        )
        .await,
    );
    errs.add(
        after_tool(
            m.context(),
            Arc::clone(&final_tool),
            answer.clone(),
            Map::new(),
            Some(Arc::new(ToolError::Execution("rejected".into()))),
        )
        .await,
    );
    errs.add(after_tool(m.context(), fake_tool("lookup"), answer, Map::new(), None).await);
    errs.check();
    after_run(&tp, &m).await;

    let rows = tp
        .collect_rows(until_event_type(EVENT_INVOCATION_COMPLETED))
        .await;
    let want_types = vec![
        EVENT_TOOL_COMPLETED,
        EVENT_AGENT_RESPONSE,
        EVENT_TOOL_ERROR,
        EVENT_TOOL_COMPLETED,
        EVENT_INVOCATION_COMPLETED,
    ];
    assert_eq!(want_types, event_types(&rows), "event types mismatch");
    let row = only_row_of_type(&rows, EVENT_AGENT_RESPONSE);
    check_json_eq(
        "content",
        &content_of(&row),
        r#"{"response":{"answer":"42"}}"#,
    );
    let attrs = json_object_column(&row, COLUMN_ATTRIBUTES);
    check_json_at(&attrs, r#""final_answer""#, &["source_tool"]);
    check_json_at(&attrs, r#""call-7""#, &["adk", "function_call_id"]);
    assert_eq!(str_of(&row, "status").as_deref(), Some("OK"), "status");
}

// TestToolFailure_WritesErrorRows replays the callbacks ADK runs for a failed tool call:
// BeforeToolCallback, OnToolErrorCallback, then AfterToolCallback with the same error. The call
// ends with a single TOOL_ERROR row, as in adk-python, which runs no after_tool_callback for
// failed calls.
#[tokio::test]
async fn test_tool_failure_writes_error_rows() {
    let tp = setup_test_plugin().await;
    let mut m = tp.ctx.clone();
    m.function_call_id = "call-1".into();
    let m = Mc::new(&m);
    let lookup_tool = fake_tool("lookup");
    let args = json!({"query": "weather"}).as_object().unwrap().clone();
    let tool_err = Arc::new(ToolError::Execution("lookup failed".into()));
    let p = &tp.plugin;
    let mut errs = CallbackErrors::default();
    errs.add(
        (p.before_tool_callback().unwrap())(m.context(), Arc::clone(&lookup_tool), args.clone())
            .await,
    );
    errs.add(
        (p.on_tool_error_callback().unwrap())(
            m.context(),
            Arc::clone(&lookup_tool),
            args.clone(),
            Arc::clone(&tool_err),
        )
        .await,
    );
    errs.add(
        (p.after_tool_callback().unwrap())(
            m.context(),
            Arc::clone(&lookup_tool),
            args,
            Map::new(),
            Some(tool_err),
        )
        .await,
    );
    errs.check();
    after_run(&tp, &m).await;

    let rows = tp
        .collect_rows(until_event_type(EVENT_INVOCATION_COMPLETED))
        .await;
    assert_eq!(
        vec![
            EVENT_TOOL_STARTING,
            EVENT_TOOL_ERROR,
            EVENT_INVOCATION_COMPLETED
        ],
        event_types(&rows),
        "event types mismatch"
    );
    let row = only_row_of_type(&rows, EVENT_TOOL_ERROR);
    assert_eq!(str_of(&row, "status").as_deref(), Some("ERROR"), "status");
    assert_eq!(
        str_of(&row, "error_message").as_deref(),
        Some("lookup failed"),
        "error_message"
    );
    check_json_eq(
        "content",
        &content_of(&row),
        r#"{"tool":"lookup","args":{"query":"weather"},"tool_origin":"UNKNOWN"}"#,
    );
    let start_span = only_row_of_type(&rows, EVENT_TOOL_STARTING)
        .get("span_id")
        .cloned();
    assert_eq!(
        row.get("span_id").cloned(),
        start_span,
        "TOOL_ERROR span_id, want the TOOL_STARTING span_id"
    );
    assert!(
        row.contains_key(COLUMN_LATENCY_MS),
        "TOOL_ERROR row has no latency_ms"
    );
}
