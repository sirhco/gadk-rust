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

//! `gcs_offloader_test.go`: the tests that drive the plugin (its constructors, callbacks and
//! Close) that 7b-2 deferred to 7b-3. Go's `offloadingPlugin` / `setupTestPluginWithConfig` are
//! [`offloading_plugin`] / `new_test_plugin_with_config` (the plugin over `FakeWriteServer`);
//! Go's single `mCtx` is one [`Mc`] (one activation) per test; Go's span context in `mCtx.ctx`
//! is [`SpanPlugin`], the production callback bodies called with `CallbackCtx::with_span`
//! (gadk reads the ambient span from the tracing subscriber). Go's `fakeStorage` is the
//! parent module's `FakeStorage`, reached by the plugin's own Cloud Storage client through a
//! [`RecordingTransport`].

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use gadk_agent::AgentError;
use gadk_genai::{
    Blob, Content, FileData, FunctionCall, FunctionResponse, GenerateContentConfig,
    GenerateContentResponseUsageMetadata, Part,
};
use gadk_model::{BodyStream, HttpClient, LlmRequest, LlmResponse, ModelError, ReqwestClient};
use pretty_assertions::assert_eq;
use serde_json::{Map, Value, json};
use tokio::time::Instant;

use super::super::plugin_agentanalytics_batch_processor::support::{
    Delayed, HookOffloader, delayed_server, image_content, requests, row_count,
};
use super::super::plugin_agentanalytics_bigquery_agent_analytics_plugin::support::{
    Mc, MockCtx, SpanPlugin, TestPlugin, check_json, fake_tool, json_object_column, lookup,
    new_test_plugin_with_config, only_row_of_type, rows_of_type, span_ids, test_bq_client,
    until_event_type,
};
use super::{FakeStorage, as_log, as_off, hooked, wait_done};
use crate::bigquery::fake::RecordingLogger;
use crate::config::{ClientOptions, Config, ContentFormatter};
use crate::context::Context;
use crate::error::Error;
use crate::event_types::{
    EVENT_INVOCATION_COMPLETED, EVENT_INVOCATION_ERROR, EVENT_LLM_ERROR, EVENT_LLM_REQUEST,
    EVENT_LLM_RESPONSE, EVENT_TOOL_STARTING, EVENT_USER_MESSAGE_RECEIVED,
};
use crate::formatter::{TEXT_UPLOAD_CONTENT_TYPE, UPLOAD_FAILED_TEXT};
use crate::offload::OwnedUploadError;
use crate::offload::uploads::safe_upload_error_summary;
use crate::plugin::new_bigquery_agent_analytics_plugin_with_clients;
use crate::redaction::{REDACTED_VALUE, UNPARSEABLE_JSON_PLACEHOLDER};
use crate::row::{Row, RowExt, RowValue};
use crate::storagepb::append_rows_request;
use crate::testutil::fake_write_server::FakeWriteServer;

// ---------------------------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------------------------

/// Go `p.BeforeModelCallback()(mCtx, req)`, which must not fail.
pub(crate) async fn before_model(tp: &TestPlugin, mc: &Mc, req: LlmRequest) {
    let (_, res) = (tp.plugin.before_model_callback().unwrap())(mc.ctx.clone(), req).await;
    res.unwrap_or_else(|e| panic!("BeforeModelCallback() error: {e}"));
}

/// Go `p.AfterModelCallback()(mCtx, res, nil)`, which must not fail.
async fn after_model(tp: &TestPlugin, mc: &Mc, res: LlmResponse) {
    let (_, r) = (tp.plugin.after_model_callback().unwrap())(mc.ctx.clone(), res, None).await;
    r.unwrap_or_else(|e| panic!("AfterModelCallback() error: {e}"));
}

/// Go `p.OnUserMessageCallback()(mCtx, msg)`, which must not fail.
pub(crate) async fn on_user_message(tp: &TestPlugin, mc: &Mc, msg: Content) {
    (tp.plugin.on_user_message_callback().unwrap())(mc.inv.clone(), msg)
        .await
        .unwrap_or_else(|e| panic!("OnUserMessageCallback() error: {e}"));
}

/// Go `p.AfterRunCallback()(mCtx)`.
pub(crate) async fn after_run(tp: &TestPlugin, mc: &Mc) {
    (tp.plugin.after_run_callback().unwrap())(mc.inv.clone()).await;
}

/// Go `offloadingPlugin(t, off, mutate)`: `setupTestPluginWithConfig` with an offloader and
/// quick background flushes, so that flushes overlap with the callbacks.
async fn offloading_plugin(
    off: &Arc<HookOffloader>,
    mutate: impl FnOnce(&mut Config),
) -> TestPlugin {
    new_test_plugin_with_config(|c| {
        c.gcs_offloader = as_off(off);
        c.batch_flush_intv = Duration::from_millis(10);
        mutate(c);
    })
    .await
}

/// Go `logRequest(t, p, requests, mCtx, req)`: the LLM_REQUEST row written once the
/// invocation ends.
async fn log_request(tp: &TestPlugin, mc: &Mc, req: LlmRequest) -> Row {
    before_model(tp, mc, req).await;
    after_run(tp, mc).await;
    let rows = tp
        .collect_rows(until_event_type(EVENT_INVOCATION_COMPLETED))
        .await;
    only_row_of_type(&rows, EVENT_LLM_REQUEST)
}

/// Go `contentPartsOf(t, row)`: the `content_parts` of a written row, which it must have.
pub(crate) fn content_parts_of(row: &Row) -> Vec<Row> {
    row.parts("content_parts")
        .map(<[Row]>::to_vec)
        .unwrap_or_else(|| panic!("the {:?} row has no content_parts", row.str("event_type")))
}

/// Go `rowDate(row)`: the UTC date of the row's timestamp, which is the date in the names of
/// its objects.
fn row_date(row: &Row) -> String {
    row.timestamp("timestamp")
        .map(|ts| ts.format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

/// A string column of a row, or "" (Go `fmt.Sprint(row[key])` of a string).
fn s(row: &Row, key: &str) -> String {
    row.str(key).unwrap_or_default().to_owned()
}

/// A Go `map[string]any` literal of a decoded row.
fn m(pairs: &[(&str, RowValue)]) -> Row {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), v.clone()))
        .collect()
}

/// Go `textPart(text, index)`.
fn text_part(text: &str, index: i64) -> Row {
    m(&[
        ("mime_type", "text/plain".into()),
        ("text", text.into()),
        ("part_index", index.into()),
        ("storage_mode", "INLINE".into()),
    ])
}

/// Go `inlineImagePart(index)`: the content_parts entry of an image that stays inline.
fn inline_image_part(index: i64) -> Row {
    m(&[
        ("mime_type", "image/png".into()),
        ("text", "[BINARY DATA]".into()),
        ("part_index", index.into()),
        ("storage_mode", "INLINE".into()),
    ])
}

/// Go `{Text: text}`.
fn text(text: &str) -> Part {
    Part {
        text: text.to_owned(),
        ..Part::default()
    }
}

/// Go `{InlineData: &genai.Blob{MIMEType: mime_type, Data: data}}`.
fn inline(mime_type: &str, data: &[u8]) -> Part {
    Part {
        inline_data: Some(Blob {
            mime_type: mime_type.to_owned(),
            data: Bytes::copy_from_slice(data),
            ..Blob::default()
        }),
        ..Part::default()
    }
}

/// Go `{FileData: &genai.FileData{FileURI: uri, MIMEType: mime_type}}`.
fn file(uri: &str, mime_type: &str) -> Part {
    Part {
        file_data: Some(FileData {
            file_uri: uri.to_owned(),
            mime_type: mime_type.to_owned(),
            ..FileData::default()
        }),
        ..Part::default()
    }
}

/// Go `&genai.Content{Role: "user", Parts: parts}`.
fn user(parts: Vec<Part>) -> Content {
    Content {
        role: "user".to_owned(),
        parts,
    }
}

/// Go `&model.LLMRequest{Contents: contents}`.
fn request(contents: Vec<Content>) -> LlmRequest {
    LlmRequest {
        contents,
        ..LlmRequest::default()
    }
}

/// Go `recordingTransport`: the HTTP client of the plugin's Cloud Storage client, which records
/// the requests it sends (`request <METHOD>`).
#[derive(Default)]
struct RecordingTransport {
    inner: ReqwestClient,
    events: Mutex<Vec<String>>,
}

impl RecordingTransport {
    /// Go `recorded()`.
    fn recorded(&self) -> Vec<String> {
        self.events.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl HttpClient for RecordingTransport {
    async fn send(
        &self,
        req: http::Request<Bytes>,
    ) -> Result<http::Response<BodyStream>, ModelError> {
        self.events
            .lock()
            .unwrap()
            .push(format!("request {}", req.method()));
        self.inner.send(req).await
    }
}

/// Go `storageClientOptions(fs)`: the options that point the Cloud Storage client of a plugin
/// at `fs`, and the transport that records what that client does.
fn storage_client_options(fs: &FakeStorage) -> (ClientOptions, Arc<RecordingTransport>) {
    let rt = Arc::new(RecordingTransport::default());
    let options = ClientOptions {
        endpoint: Some(format!("{}/storage/v1/", fs.server.url())),
        without_authentication: true,
        http_client: Some(Arc::clone(&rt) as Arc<dyn HttpClient>),
        ..ClientOptions::default()
    };
    (options, rt)
}

/// Options with which no Cloud Storage client can be made (Go's
/// `option.WithCredentialsFile(<missing file>)`): an endpoint that does not parse.
fn bad_client_options() -> ClientOptions {
    ClientOptions {
        endpoint: Some("http://[::1".into()),
        ..ClientOptions::default()
    }
}

/// Go `req.GetArrowRows().GetRows().GetSerializedRecordBatch()`'s length.
fn serialized_record_batch_len(req: &crate::storagepb::AppendRowsRequest) -> usize {
    match &req.rows {
        Some(append_rows_request::Rows::ArrowRows(data)) => data
            .rows
            .as_ref()
            .map_or(0, |r| r.serialized_record_batch.len()),
        _ => 0,
    }
}

/// Go `select { case req = <-requests: … case <-time.After(5 * time.Second): t.Fatal(…) }`.
async fn first_request(tp: &TestPlugin) -> crate::storagepb::AppendRowsRequest {
    tokio::time::timeout(
        Duration::from_secs(5),
        tp.server.wait_for_append_requests(1),
    )
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for the batch to be written"));
    tp.server.append_requests()[0].request.clone()
}

/// Go `fakeOffloader.hook` that waits for its context: `<-ctx.Done(); return ctx.Err()`.
fn hanging_offloader() -> Arc<HookOffloader> {
    hooked(|ctx, _| Box::pin(wait_done(ctx)))
}

/// Go `fakeOffloader.hook` that fails every call with `msg`.
fn failing_offloader(msg: &'static str) -> Arc<HookOffloader> {
    hooked(move |_, _| Box::pin(async move { Err(Error::msg(msg)) }))
}

// ---------------------------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------------------------

// parity: adapted: Go's span context in mCtx.ctx → the callback bodies called with CallbackCtx::with_span (SpanPlugin)
#[tokio::test(start_paused = true)]
async fn test_gcs_offloading_hybrid_inline_and_gcs_reference() {
    let off = Arc::new(HookOffloader::default());
    let sp = SpanPlugin::new(|c| {
        c.gcs_offloader = as_off(&off);
        c.batch_flush_intv = Duration::from_millis(10);
        c.gcs_path_prefix = "logs/".into();
        c.connection_id = "us.my-connection".into();
        c.max_inline_text_size = 40;
    })
    .await;
    const TRACE_ID: &str = "4bf92f3577b34da6a3ce929d0e0e4736";
    const SPAN_ID: &str = "00f067aa0ba902b7";
    let ctx = SpanPlugin::ctx(&sp.tp.ctx, span_ids(TRACE_ID, SPAN_ID));
    let long_text = "0123456789".repeat(10);
    let png: &[u8] = &[0x89, b'P', b'N', b'G', 1, 2, 3];
    let req = LlmRequest {
        model: "gemini-test".into(),
        contents: vec![
            user(vec![text("short"), text(&long_text)]),
            user(vec![
                inline("image/png", png),
                file("gs://bucket/existing.pdf", "application/pdf"),
                file("https://example.com/doc.pdf", "application/pdf"),
            ]),
        ],
        config: Some(GenerateContentConfig {
            system_instruction: Some(Content::from_text("be brief", "")),
            ..GenerateContentConfig::default()
        }),
        ..LlmRequest::default()
    };

    sp.ap.before_model(&ctx, &req);
    sp.after_run(&ctx).await;
    let rows = sp
        .tp
        .collect_rows(until_event_type(EVENT_INVOCATION_COMPLETED))
        .await;
    let row = only_row_of_type(&rows, EVENT_LLM_REQUEST);

    assert!(
        s(&row, "trace_id") == TRACE_ID && !s(&row, "span_id").is_empty(),
        "row trace_id={:?} span_id={:?}, want trace_id={TRACE_ID} and non-empty span_id",
        row.get("trace_id"),
        row.get("span_id")
    );
    let base = format!(
        "logs/{}/{TRACE_ID}/{}_{}",
        row_date(&row),
        s(&row, "span_id"),
        s(&row, "event_id")
    );
    let (text_object, image_object) = (format!("{base}_c0_p1.txt"), format!("{base}_c1_p0.png"));
    let text_uri = format!("gs://fake-bucket/{text_object}");
    let image_uri = format!("gs://fake-bucket/{image_object}");
    let want = vec![
        text_part("short", 0),
        m(&[
            ("part_index", 1_i64.into()),
            ("storage_mode", "GCS_REFERENCE".into()),
            ("mime_type", "text/plain".into()),
            ("text", format!("{long_text}... [OFFLOADED]").into()),
            ("uri", text_uri.as_str().into()),
            (
                "object_ref",
                m(&[
                    ("uri", text_uri.as_str().into()),
                    ("authorizer", "us.my-connection".into()),
                    (
                        "details",
                        r#"{"gcs_metadata":{"content_type":"text/plain; charset=utf-8","size":100}}"#
                            .into(),
                    ),
                ])
                .into(),
            ),
        ]),
        m(&[
            ("part_index", 0_i64.into()),
            ("storage_mode", "GCS_REFERENCE".into()),
            ("mime_type", "image/png".into()),
            ("text", "[MEDIA OFFLOADED]".into()),
            ("uri", image_uri.as_str().into()),
            (
                "object_ref",
                m(&[
                    ("uri", image_uri.as_str().into()),
                    ("authorizer", "us.my-connection".into()),
                    (
                        "details",
                        r#"{"gcs_metadata":{"content_type":"image/png","size":7}}"#.into(),
                    ),
                ])
                .into(),
            ),
        ]),
        m(&[
            ("part_index", 1_i64.into()),
            ("storage_mode", "EXTERNAL_URI".into()),
            ("mime_type", "application/pdf".into()),
            ("uri", "gs://bucket/existing.pdf".into()),
            (
                "object_ref",
                m(&[
                    ("uri", "gs://bucket/existing.pdf".into()),
                    ("authorizer", "us.my-connection".into()),
                    ("details", "{}".into()),
                ])
                .into(),
            ),
        ]),
        m(&[
            ("part_index", 2_i64.into()),
            ("storage_mode", "EXTERNAL_URI".into()),
            ("mime_type", "application/pdf".into()),
            ("uri", "https://example.com/doc.pdf".into()),
        ]),
        text_part("be brief", 0),
    ];
    assert_eq!(content_parts_of(&row), want, "content_parts mismatch");
    let got: Vec<(String, String, Bytes)> = off
        .uploads()
        .into_iter()
        .map(|u| (u.path, u.content_type, u.data))
        .collect();
    let want_uploads = vec![
        (
            text_object.clone(),
            TEXT_UPLOAD_CONTENT_TYPE.to_owned(),
            Bytes::from(long_text.clone()),
        ),
        (
            image_object.clone(),
            "image/png".to_owned(),
            Bytes::copy_from_slice(png),
        ),
    ];
    assert_eq!(got, want_uploads, "uploads mismatch");
    assert_eq!(
        row.bool("is_truncated"),
        Some(false),
        "is_truncated, want false: nothing was cut"
    );
}

#[tokio::test(start_paused = true)]
async fn test_gcs_offloading_without_an_offloader_everything_stays_inline() {
    // A small MaxInlineTextSize alone does not turn offloading on.
    let tp = new_test_plugin_with_config(|c| c.max_inline_text_size = 10).await;
    let mc = Mc::of(&tp.ctx);
    let long_text = "x".repeat(100);
    let req = request(vec![user(vec![
        text(&long_text),
        inline("image/png", &[1]),
        file("gs://bucket/a.pdf", "application/pdf"),
    ])]);

    let row = log_request(&tp, &mc, req).await;

    let want = vec![
        text_part(&long_text, 0),
        inline_image_part(1),
        m(&[
            ("part_index", 2_i64.into()),
            ("storage_mode", "EXTERNAL_URI".into()),
            ("mime_type", "application/pdf".into()),
            ("uri", "gs://bucket/a.pdf".into()),
        ]),
    ];
    assert_eq!(content_parts_of(&row), want, "content_parts mismatch");
}

// parity: adapted: gadk's ContentFormatter maps the payload's JSON (an LLMRequest as Go's json.Marshal of model.LLMRequest) to JSON, so the formatter rebuilds the Contents of that JSON
#[tokio::test(start_paused = true)]
async fn test_gcs_offloading_content_formatter_runs_before_the_upload() {
    let off = Arc::new(HookOffloader::default());
    // This formatter hides a secret in the text of the request.
    let formatter: ContentFormatter = Arc::new(|payload: &Value, _: &str| {
        let Some(contents) = payload.get("Contents").and_then(Value::as_array) else {
            return Ok(payload.clone());
        };
        let out: Vec<Value> = contents
            .iter()
            .map(|content| {
                let parts: Vec<Value> = content
                    .get("parts")
                    .and_then(Value::as_array)
                    .map(|parts| {
                        parts
                            .iter()
                            .map(|part| {
                                let t = part.get("text").and_then(Value::as_str).unwrap_or("");
                                json!({"text": t.replace("sk-secret-123", "[HIDDEN]")})
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                json!({"role": content.get("role").cloned().unwrap_or(Value::Null), "parts": parts})
            })
            .collect();
        Ok(json!({"Contents": out}))
    });
    let tp = offloading_plugin(&off, |c| {
        c.max_inline_text_size = 20;
        c.content_formatter = Some(formatter);
    })
    .await;
    let mc = Mc::of(&tp.ctx);
    let t = format!("my key is sk-secret-123 {}", "padding ".repeat(20));

    let row = log_request(&tp, &mc, request(vec![Content::from_text(&t, "user")])).await;

    let uploads = off.uploads();
    assert_eq!(uploads.len(), 1, "got {} uploads, want 1", uploads.len());
    let got = String::from_utf8_lossy(&uploads[0].data).into_owned();
    assert!(
        !got.contains("sk-secret-123") && got.contains("[HIDDEN]"),
        "the uploaded text is {got:?}, want the formatter's version"
    );
    let got = s(&content_parts_of(&row)[0], "text");
    assert!(
        !got.contains("sk-secret-123") && got.contains("[HIDDEN]"),
        "the preview is {got:?}, want the formatter's version"
    );
}

// parity: adapted: Go's "error type *errors.errorString" is gadk's Go-type name of an Error::msg (as the 7b-2 safeUploadErrorSummary port)
#[tokio::test(start_paused = true)]
async fn test_gcs_offloading_upload_failure_keeps_the_parts_inline() {
    let off = failing_offloader("storage unavailable: SECRET-ERR-MARKER");
    let logger = Arc::new(RecordingLogger::default());
    let tp = offloading_plugin(&off, |c| {
        c.max_inline_text_size = 20;
        c.logger = as_log(&logger);
    })
    .await;
    let mc = Mc::of(&tp.ctx);
    let long_text = format!("SECRET-TEXT-MARKER{}", "-".repeat(50));
    let req = request(vec![user(vec![
        text(&long_text),
        inline("image/png", b"SECRET-IMAGE-MARKER"),
    ])]);

    let row = log_request(&tp, &mc, req).await;

    let mut failed_image = inline_image_part(1);
    failed_image.insert("text".into(), UPLOAD_FAILED_TEXT.into());
    let want = vec![text_part(&long_text, 0), failed_image];
    assert_eq!(content_parts_of(&row), want, "content_parts mismatch");
    assert_eq!(row.bool("is_truncated"), Some(false), "is_truncated");
    assert!(
        logger.contains("GCS upload failed") && logger.contains("error type *errors.errorString"),
        "the log lacks the failure:\n{logger}"
    );
    for secret in [
        "SECRET-TEXT-MARKER",
        "SECRET-IMAGE-MARKER",
        "SECRET-ERR-MARKER",
    ] {
        assert!(
            !logger.contains(secret),
            "the log contains {secret:?}:\n{logger}"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn test_gcs_offloading_failed_upload_of_cut_text_keeps_the_cut() {
    let off = failing_offloader("storage unavailable");
    let tp = offloading_plugin(&off, |c| {
        c.max_content_len = 50;
        c.logger = as_log(&Arc::new(RecordingLogger::default()));
    })
    .await;
    let mc = Mc::of(&tp.ctx);
    let t = "0123456789".repeat(12);

    let row = log_request(&tp, &mc, request(vec![Content::from_text(&t, "user")])).await;

    let want = vec![text_part(&format!("{}...[TRUNCATED]", &t[..50]), 0)];
    assert_eq!(content_parts_of(&row), want, "content_parts mismatch");
    assert_eq!(
        row.bool("is_truncated"),
        Some(true),
        "is_truncated, want true: the row holds a cut of the text"
    );
}

#[tokio::test(start_paused = true)]
async fn test_gcs_offloading_text_cut_by_max_content_len_is_offloaded_in_full() {
    let off = Arc::new(HookOffloader::default());
    // The text is within the inline limit, but a row cannot keep all of it inline:
    // MaxContentLen would cut it.
    let tp = offloading_plugin(&off, |c| c.max_content_len = 50).await;
    let mc = Mc::of(&tp.ctx);
    let t = "0123456789".repeat(12);

    let row = log_request(&tp, &mc, request(vec![Content::from_text(&t, "user")])).await;

    let uploads = off.uploads();
    assert!(
        uploads.len() == 1 && uploads[0].data == t.as_bytes(),
        "uploads = {uploads:?}, want one with all of the text"
    );
    let part = &content_parts_of(&row)[0];
    assert!(
        s(part, "storage_mode") == "GCS_REFERENCE"
            && s(part, "text") == format!("{t}... [OFFLOADED]"),
        "the part is {part:?}, want a reference with the preview of the whole text"
    );
    assert_eq!(
        row.bool("is_truncated"),
        Some(false),
        "is_truncated, want false: the full text was offloaded"
    );
}

#[tokio::test(start_paused = true)]
async fn test_gcs_offloading_rows_without_content_upload_nothing() {
    let off = Arc::new(HookOffloader::default());
    let tp = offloading_plugin(&off, |_| {}).await;
    let mc = Mc::of(&tp.ctx);
    // An LLM_ERROR row has no content, although the request holds an image.
    let req = LlmRequest {
        model: "gemini-test".into(),
        contents: vec![image_content(&[1, 2, 3])],
        ..LlmRequest::default()
    };
    (tp.plugin.on_model_error_callback().unwrap())(
        mc.ctx.clone(),
        req,
        Arc::new(ModelError::Other("model unavailable".into())),
    )
    .await
    .unwrap_or_else(|e| panic!("OnModelErrorCallback() error: {e}"));
    after_run(&tp, &mc).await;

    let rows = tp
        .collect_rows(until_event_type(EVENT_INVOCATION_ERROR))
        .await;

    let row = only_row_of_type(&rows, EVENT_LLM_ERROR);
    assert!(
        s(&row, "status") == "ERROR" && s(&row, "error_message") == "model unavailable",
        "status = {:?}, error_message = {:?}; want ERROR and the error",
        row.get("status"),
        row.get("error_message")
    );
    assert!(row.contains_key("attributes"), "the row has no attributes");
    assert!(
        !row.contains_key("content_parts"),
        "the row has content_parts {:?}, want none",
        row.get("content_parts")
    );
    let got = off.uploads();
    assert!(got.is_empty(), "got uploads {got:?}, want none");
}

#[tokio::test(start_paused = true)]
async fn test_gcs_offloading_panicking_offloader_loses_no_rows() {
    let off = hooked(|_, _| Box::pin(async { panic!("PANIC-MARKER") }));
    let logger = Arc::new(RecordingLogger::default());
    let tp = offloading_plugin(&off, |c| c.logger = as_log(&logger)).await;
    let mc = Mc::of(&tp.ctx);

    let row = log_request(&tp, &mc, request(vec![image_content(&[1])])).await;

    let mut failed_image = inline_image_part(0);
    failed_image.insert("text".into(), UPLOAD_FAILED_TEXT.into());
    assert_eq!(
        content_parts_of(&row),
        vec![failed_image],
        "content_parts mismatch"
    );
    assert!(
        logger.contains("offloader panicked (string)"),
        "the log does not say that the offloader panicked:\n{logger}"
    );
    assert!(
        !logger.contains("PANIC-MARKER"),
        "the log contains the panic value:\n{logger}"
    );
}

// parity: adapted: a Rust request cannot hold Go's nil *Content, so the second message is an empty Content (no parts; it keeps its number as Go's nil message does)
#[tokio::test(start_paused = true)]
async fn test_gcs_offloading_object_names_tell_the_messages_apart() {
    let off = Arc::new(HookOffloader::default());
    let tp = offloading_plugin(&off, |_| {}).await;
    let mc = Mc::of(&tp.ctx);
    // Every message has its image at part index 0. A nil message keeps its number, and the
    // system instruction comes after all of the messages.
    let req = LlmRequest {
        contents: vec![image_content(&[1]), Content::default(), image_content(&[3])],
        config: Some(GenerateContentConfig {
            system_instruction: Some(image_content(&[4])),
            ..GenerateContentConfig::default()
        }),
        ..LlmRequest::default()
    };

    let row = log_request(&tp, &mc, req).await;

    let base = format!(
        "{}/{}/{}_{}",
        row_date(&row),
        s(&row, "trace_id"),
        s(&row, "span_id"),
        s(&row, "event_id")
    );
    let want: BTreeMap<String, Bytes> = [
        (format!("{base}_c0_p0.png"), Bytes::from_static(&[1])),
        (format!("{base}_c2_p0.png"), Bytes::from_static(&[3])),
        (format!("{base}_c3_p0.png"), Bytes::from_static(&[4])),
    ]
    .into();
    let got: BTreeMap<String, Bytes> = off
        .uploads()
        .into_iter()
        .map(|u| (u.path, u.data))
        .collect();
    assert_eq!(got, want, "uploaded objects mismatch");
}

// parity: adapted: Rust callbacks take their messages by value, so the caller's later changes reach only its own copies; the test keeps Go's shape (the upload waits while the caller changes its messages, in real time) and its checks
#[tokio::test]
async fn test_gcs_offloading_caller_changes_its_messages_while_rows_are_written() {
    let passes = Arc::new(AtomicI64::new(0));
    let off = {
        let passes = Arc::clone(&passes);
        hooked(move |_, _| {
            let passes = Arc::clone(&passes);
            Box::pin(async move {
                // Hold the upload until the caller has been at work on its messages for a
                // while. The offloader reads the data after this.
                let deadline = Instant::now() + Duration::from_secs(2);
                while passes.load(Ordering::SeqCst) < 20 && Instant::now() < deadline {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
                Ok(())
            })
        })
    };
    let tp = offloading_plugin(&off, |c| c.max_inline_text_size = 50).await;
    let mc = Mc::of(&tp.ctx);

    let pristine_blob: &[u8] = &[1, 2, 3, 4, 5, 6, 7, 8];
    let long_text = "abcdefghij".repeat(30);
    let mut req = request(vec![
        user(vec![text(&long_text), inline("image/png", pristine_blob)]),
        user(vec![Part {
            function_response: Some(FunctionResponse {
                name: "lookup".into(),
                response: json!({"k": 1000}).as_object().unwrap().clone(),
                ..FunctionResponse::default()
            }),
            ..Part::default()
        }]),
    ]);
    let mut args: Map<String, Value> = json!({"q": "pristine"}).as_object().unwrap().clone();
    before_model(&tp, &mc, req.clone()).await;
    (tp.plugin.before_tool_callback().unwrap())(mc.ctx.clone(), fake_tool("lookup"), args.clone())
        .await
        .unwrap_or_else(|e| panic!("callback error: {e}"));

    // The callbacks have returned, so the caller is free to change its messages. Its changes
    // never stop until the rows are written.
    let stop = tokio_util::sync::CancellationToken::new();
    let mutator = {
        let (stop, passes) = (stop.clone(), Arc::clone(&passes));
        tokio::spawn(async move {
            while !stop.is_cancelled() {
                req.contents[0].parts[1].inline_data.as_mut().unwrap().data =
                    Bytes::from_static(&[0xEE; 8]);
                req.contents[0].parts[0].text = "MUTATED".into();
                req.contents[1].parts[0]
                    .function_response
                    .as_mut()
                    .unwrap()
                    .response
                    .insert("k".into(), json!(-1));
                args.insert("q".into(), json!("mutated"));
                passes.fetch_add(1, Ordering::SeqCst);
                tokio::task::yield_now().await;
            }
        })
    };
    after_run(&tp, &mc).await;
    let rows = tp
        .collect_rows(until_event_type(EVENT_INVOCATION_COMPLETED))
        .await;
    stop.cancel();
    mutator.await.unwrap();

    // What was uploaded is what the messages held when the callback ran...
    let got: BTreeMap<String, Bytes> = off
        .uploads()
        .into_iter()
        .map(|u| {
            let at = u.path.rfind("_c").unwrap_or(0);
            (u.path[at..].to_owned(), u.data)
        })
        .collect();
    let want: BTreeMap<String, Bytes> = [
        ("_c0_p0.txt".to_owned(), Bytes::from(long_text.clone())),
        (
            "_c0_p1.png".to_owned(),
            Bytes::copy_from_slice(pristine_blob),
        ),
    ]
    .into();
    assert_eq!(got, want, "uploaded data mismatch");
    // ... and so is what the rows hold.
    let parts = content_parts_of(&only_row_of_type(&rows, EVENT_LLM_REQUEST));
    assert_eq!(parts.len(), 3, "content_parts: {parts:?}");
    let want_preview = format!("{}... [OFFLOADED]", &long_text[..200]);
    assert_eq!(s(&parts[0], "text"), want_preview, "the text preview");
    let attrs: Value = serde_json::from_str(&s(&parts[2], "part_attributes"))
        .unwrap_or_else(|e| panic!("part_attributes is not JSON: {e}"));
    check_json(
        "part_attributes",
        &attrs,
        r#"{"function_response":{"name":"lookup","response":{"k":1000}}}"#,
    )
    .unwrap_or_else(|e| panic!("{e}"));
    let tool_content = json_object_column(&only_row_of_type(&rows, EVENT_TOOL_STARTING), "content");
    check_json(
        "args",
        lookup(&tool_content, &["args"]).unwrap_or(&Value::Null),
        r#"{"q":"pristine"}"#,
    )
    .unwrap_or_else(|e| panic!("{e}"));
}

#[tokio::test(start_paused = true)]
async fn test_gcs_offloading_many_callers_and_flushes() {
    const GOROUTINES: usize = 6;
    const PER_GOROUTINE: usize = 20;
    let off = Arc::new(HookOffloader::default());
    let tp = Arc::new(
        offloading_plugin(&off, |c| {
            c.batch_size = 7;
            c.max_inline_text_size = 64;
        })
        .await,
    );
    let mc = Arc::new(Mc::of(&tp.ctx));
    let mut callers = Vec::new();
    for g in 0..GOROUTINES {
        let (tp, mc) = (Arc::clone(&tp), Arc::clone(&mc));
        callers.push(tokio::spawn(async move {
            for i in 0..PER_GOROUTINE {
                let marker = format!("g{g:02}-i{i:02}");
                let req = request(vec![user(vec![
                    text(&format!("{marker}{}", ".".repeat(100))),
                    inline("image/png", &[g as u8, i as u8]),
                ])]);
                before_model(&tp, &mc, req).await;
            }
        }));
    }
    for c in callers {
        c.await.unwrap();
    }
    after_run(&tp, &mc).await;

    let rows = tp
        .collect_rows(|rows| {
            rows_of_type(rows, EVENT_LLM_REQUEST).len() == GOROUTINES * PER_GOROUTINE
        })
        .await;

    let uploaded = off.by_path();
    let want = 2 * GOROUTINES * PER_GOROUTINE;
    let got = off.uploads().len();
    assert!(
        got == want && uploaded.len() == want,
        "{got} uploads to {} distinct objects, want {want} of each",
        uploaded.len()
    );
    for row in rows_of_type(&rows, EVENT_LLM_REQUEST) {
        let parts = content_parts_of(&row);
        assert_eq!(parts.len(), 2, "a row has content_parts: {parts:?}");
        let text_path = s(&parts[0], "uri")
            .trim_start_matches("gs://fake-bucket/")
            .to_owned();
        let image_path = s(&parts[1], "uri")
            .trim_start_matches("gs://fake-bucket/")
            .to_owned();
        // The row names its own objects, and the objects hold its own data.
        let want_prefix = format!(
            "{}/{}/{}_{}_c0_p",
            row_date(&row),
            s(&row, "trace_id"),
            s(&row, "span_id"),
            s(&row, "event_id")
        );
        assert!(
            text_path.starts_with(&want_prefix) && image_path.starts_with(&want_prefix),
            "the objects of the row are {text_path:?} and {image_path:?}, want names that start with {want_prefix:?}"
        );
        let preview = s(&parts[0], "text");
        let marker = preview.split('.').next().unwrap_or_default().to_owned();
        let (g, i) = marker
            .strip_prefix('g')
            .and_then(|r| r.split_once("-i"))
            .and_then(|(g, i)| Some((g.parse::<u8>().ok()?, i.parse::<u8>().ok()?)))
            .unwrap_or_else(|| {
                panic!("the preview {preview:?} of a row does not start with a marker")
            });
        let text_data = &uploaded[&text_path].data;
        assert!(
            text_data.starts_with(marker.as_bytes()),
            "the text object of {marker} holds {:?}",
            &text_data[..20.min(text_data.len())]
        );
        assert_eq!(
            uploaded[&image_path].data.as_ref(),
            &[g, i],
            "the image object of {marker}"
        );
    }
}

// parity: adapted: the plugin's Cloud Storage client reaches FakeStorage through ClientOptions (endpoint, no authentication, the recording HTTP client) instead of Go's STORAGE_EMULATOR_HOST-style options
#[tokio::test]
async fn test_gcs_offloading_bucket_name_builds_an_offloader() {
    let fs = FakeStorage::new();
    let (opts, _) = storage_client_options(&fs);
    // There is no GCSOffloader: the plugin builds one for the bucket.
    let tp = new_test_plugin_with_config(|c| {
        c.gcs_bucket_name = "test-bucket".into();
        c.gcs_path_prefix = "agent-logs".into();
        c.connection_id = "us.conn".into();
        c.client_options = opts;
    })
    .await;
    let mc = Mc::of(&tp.ctx);
    let png: &[u8] = &[0x89, b'P', b'N', b'G'];

    let row = log_request(&tp, &mc, request(vec![image_content(png)])).await;

    let object = format!(
        "agent-logs/{}/{}/{}_{}_c0_p0.png",
        row_date(&row),
        s(&row, "trace_id"),
        s(&row, "span_id"),
        s(&row, "event_id")
    );
    let uri = format!("gs://test-bucket/{object}");
    let want = vec![m(&[
        ("part_index", 0_i64.into()),
        ("storage_mode", "GCS_REFERENCE".into()),
        ("mime_type", "image/png".into()),
        ("text", "[MEDIA OFFLOADED]".into()),
        ("uri", uri.as_str().into()),
        (
            "object_ref",
            m(&[
                ("uri", uri.as_str().into()),
                ("authorizer", "us.conn".into()),
                (
                    "details",
                    r#"{"gcs_metadata":{"content_type":"image/png","size":4}}"#.into(),
                ),
            ])
            .into(),
        ),
    ])];
    assert_eq!(content_parts_of(&row), want, "content_parts mismatch");
    let got = fs
        .object("test-bucket", &object)
        .unwrap_or_else(|| panic!("the object {object:?} was not stored"));
    assert!(
        got.content_type == "image/png" && got.data.as_ref() == png,
        "stored object = {got:?}, want {{image/png, {png:x?}}}"
    );
    tp.close().await.unwrap();
}

// parity: adapted: gadk's Cloud Storage client has no idle connections to close (GcsOffloader::close is a no-op on its transport), so Go's "close" event is not recorded; the test checks that the last flush, which only Close runs, uploaded through the owned client
#[tokio::test]
async fn test_gcs_offloading_closes_the_owned_offloader_after_the_processor() {
    let fs = FakeStorage::new();
    let (opts, rt) = storage_client_options(&fs);
    let tp = new_test_plugin_with_config(|c| {
        c.gcs_bucket_name = "test-bucket".into();
        c.client_options = opts;
        // Nothing but closing the plugin writes the row.
        c.disable_flush_on_run_end = true;
        c.batch_flush_intv = Duration::from_secs(3600);
    })
    .await;
    let mc = Mc::of(&tp.ctx);
    before_model(&tp, &mc, request(vec![image_content(&[1])])).await;

    tp.close()
        .await
        .unwrap_or_else(|e| panic!("Close() error: {e}"));

    let rows = tp
        .collect_rows(|rows| rows_of_type(rows, EVENT_LLM_REQUEST).len() == 1)
        .await;
    let parts = content_parts_of(&only_row_of_type(&rows, EVENT_LLM_REQUEST));
    assert!(
        parts.len() == 1 && s(&parts[0], "storage_mode") == "GCS_REFERENCE",
        "content_parts = {parts:?}, want the image to be uploaded by the last flush"
    );
    let events = rt.recorded();
    assert!(
        events.iter().any(|e| e == "request POST"),
        "the Cloud Storage client saw {events:?}, want its upload"
    );
}

#[tokio::test]
async fn test_gcs_offloading_supplied_offloader_wins_and_is_not_closed() {
    let fs = FakeStorage::new();
    let (opts, rt) = storage_client_options(&fs);
    let off = Arc::new(HookOffloader::default());
    let tp = offloading_plugin(&off, |c| {
        c.gcs_bucket_name = "ignored-bucket".into();
        c.client_options = opts;
    })
    .await;
    let mc = Mc::of(&tp.ctx);

    let row = log_request(&tp, &mc, request(vec![image_content(&[1])])).await;
    tp.close()
        .await
        .unwrap_or_else(|e| panic!("Close() error: {e}"));

    let uri = s(&content_parts_of(&row)[0], "uri");
    assert!(
        uri.starts_with("gs://fake-bucket/"),
        "uri = {uri:?}, want an object of the supplied offloader"
    );
    assert_eq!(
        off.close_count(),
        0,
        "the supplied offloader was closed; it belongs to the caller"
    );
    let got = rt.recorded();
    assert!(
        got.is_empty(),
        "the Cloud Storage client was used ({got:?}) although an offloader was supplied"
    );
}

// parity: adapted: no Cloud Storage client can be made from options whose endpoint does not parse (Go: a missing credentials file); gadk's WriteClient::close never fails, so "closing it once more fails" is the client's closed flag
#[tokio::test]
async fn test_gcs_offloading_offloader_creation_failure_closes_the_write_client() {
    let server = FakeWriteServer::new();
    let write_client = server.client().unwrap();
    let mut cfg = Config::default();
    cfg.project_id = "test-project".into();
    cfg.gcs_bucket_name = "test-bucket".into();
    cfg.logger = as_log(&Arc::new(RecordingLogger::default()));
    // The endpoint does not parse, so no Cloud Storage client can be made.
    cfg.client_options = bad_client_options();

    let result = new_bigquery_agent_analytics_plugin_with_clients(
        &Context::background(),
        cfg,
        None,
        Some(write_client.clone()),
    )
    .await;

    let err = match result {
        Ok(p) => {
            let _ = (p.close().unwrap())().await;
            panic!("NewBigQueryAgentAnalyticsPluginWithClients() succeeded, want an error");
        }
        Err(err) => err,
    };
    assert!(
        err.to_string().contains("failed to create GCS offloader"),
        "error = {err}, want one about the GCS offloader"
    );
    // The write client is the plugin's from the call on, so a failed construction closes it.
    assert!(
        write_client.closed.load(Ordering::SeqCst),
        "the write client is still open after the failed construction"
    );
}

// parity: adapted: gadk runs Close once (ledgered R5 divergence: a second Close returns the first's result) and its clients' close never fails, so the second Close returns the first's Ok instead of Go's "failed to close BigQuery write client"
#[tokio::test]
async fn test_gcs_offloading_close_reports_the_close_errors() {
    let fs = FakeStorage::new();
    let (opts, _) = storage_client_options(&fs);
    let tp = new_test_plugin_with_config(|c| {
        c.gcs_bucket_name = "test-bucket".into();
        c.client_options = opts;
    })
    .await;

    tp.close()
        .await
        .unwrap_or_else(|e| panic!("Close() error: {e}"));

    let second = tp.close().await;
    assert!(
        second.is_ok(),
        "closing the plugin again returned {second:?}, want the first Close's result"
    );
}

#[tokio::test(start_paused = true)]
async fn test_gcs_offloading_redacts_sensitive_json_before_upload() {
    let off = Arc::new(HookOffloader::default());
    let tp = offloading_plugin(&off, |c| c.max_inline_text_size = 20).await;
    let mc = Mc::of(&tp.ctx);
    let secret_payload = format!(
        r#"{{"api_key":"sk-live-secret-999","note":"{}"}}"#,
        "z".repeat(80)
    );

    let row = log_request(
        &tp,
        &mc,
        request(vec![Content::from_text(&secret_payload, "user")]),
    )
    .await;

    let uploads = off.uploads();
    assert_eq!(uploads.len(), 1, "got {} uploads, want 1", uploads.len());
    let got = String::from_utf8_lossy(&uploads[0].data).into_owned();
    assert!(
        !got.contains("sk-live-secret-999") && got.contains(REDACTED_VALUE),
        "uploaded text = {got:?}, want sensitive key redacted before GCS upload"
    );
    let preview = s(&content_parts_of(&row)[0], "text");
    assert!(
        !preview.contains("sk-live-secret-999") && preview.contains(REDACTED_VALUE),
        "inline preview = {preview:?}, want sensitive key redacted"
    );
}

#[tokio::test(start_paused = true)]
async fn test_gcs_offloading_omits_offloaded_text_from_content_summary() {
    let limits = |c: &mut Config| {
        c.max_inline_text_size = 40;
        c.max_content_len = 60;
    };
    {
        let name = "successful upload omits offloaded text from USER_MESSAGE_RECEIVED and LLM_REQUEST summaries";
        let off = Arc::new(HookOffloader::default());
        let tp = offloading_plugin(&off, limits).await;
        let mc = Mc::of(&tp.ctx);
        let offloaded_body = format!("OFFLOADED-BODY-{}", "A".repeat(120));
        let user_msg = user(vec![text("keep-me"), text(&offloaded_body)]);
        on_user_message(&tp, &mc, user_msg.clone()).await;
        let sys_offloaded = format!("OFFLOADED-SYS-{}", "B".repeat(120));
        let req = LlmRequest {
            contents: vec![user_msg],
            config: Some(GenerateContentConfig {
                system_instruction: Some(Content::from_text(&sys_offloaded, "")),
                ..GenerateContentConfig::default()
            }),
            ..LlmRequest::default()
        };
        before_model(&tp, &mc, req).await;
        after_run(&tp, &mc).await;

        let rows = tp
            .collect_rows(until_event_type(EVENT_INVOCATION_COMPLETED))
            .await;

        let user_row = only_row_of_type(&rows, EVENT_USER_MESSAGE_RECEIVED);
        let user_content = json_object_column(&user_row, "content");
        assert_eq!(
            user_content["text_summary"], "keep-me",
            "{name}: USER_MESSAGE_RECEIVED text_summary (offloaded text omitted)"
        );
        assert_eq!(
            user_row.bool("is_truncated"),
            Some(false),
            "{name}: USER_MESSAGE_RECEIVED is_truncated"
        );
        let llm_row = only_row_of_type(&rows, EVENT_LLM_REQUEST);
        let llm_content = json_object_column(&llm_row, "content");
        check_json(
            "prompt",
            lookup(&llm_content, &["prompt"]).unwrap_or(&Value::Null),
            r#"[{"role":"user","content":"keep-me"}]"#,
        )
        .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(
            llm_content["system_prompt"], "",
            "{name}: LLM_REQUEST system_prompt, want empty when system prompt was offloaded"
        );
        assert_eq!(
            llm_row.bool("is_truncated"),
            Some(false),
            "{name}: LLM_REQUEST is_truncated"
        );
    }
    {
        let name = "successful upload omits offloaded text from LLM_RESPONSE while keeping non-text parts and usage";
        let off = Arc::new(HookOffloader::default());
        let tp = offloading_plugin(&off, limits).await;
        let mc = Mc::of(&tp.ctx);
        let offloaded_resp = format!("OFFLOADED-RESP-{}", "C".repeat(120));
        let resp = LlmResponse {
            content: Some(Content {
                role: "model".into(),
                parts: vec![
                    text(&offloaded_resp),
                    Part {
                        function_call: Some(FunctionCall {
                            name: "lookup".into(),
                            ..FunctionCall::default()
                        }),
                        ..Part::default()
                    },
                ],
            }),
            usage_metadata: Some(GenerateContentResponseUsageMetadata {
                prompt_token_count: 10,
                candidates_token_count: 20,
                total_token_count: 30,
                ..GenerateContentResponseUsageMetadata::default()
            }),
            ..LlmResponse::default()
        };
        after_model(&tp, &mc, resp).await;
        after_run(&tp, &mc).await;

        let rows = tp
            .collect_rows(until_event_type(EVENT_INVOCATION_COMPLETED))
            .await;
        let resp_row = only_row_of_type(&rows, EVENT_LLM_RESPONSE);
        let resp_content = json_object_column(&resp_row, "content");
        assert_eq!(
            resp_content["response"], "call: lookup",
            "{name}: LLM_RESPONSE content.response (offloaded text omitted)"
        );
        check_json(
            "usage",
            lookup(&resp_content, &["usage"]).unwrap_or(&Value::Null),
            r#"{"prompt":10,"completion":20,"total":30}"#,
        )
        .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(
            resp_row.bool("is_truncated"),
            Some(false),
            "{name}: LLM_RESPONSE is_truncated"
        );
    }
    {
        let name = "failed upload retains truncated inline text in summary and marks row truncated";
        let off = failing_offloader("boom");
        let tp = offloading_plugin(&off, |c| {
            limits(c);
            c.logger = as_log(&Arc::new(RecordingLogger::default()));
        })
        .await;
        let mc = Mc::of(&tp.ctx);
        let offloaded_body = format!("OFFLOADED-BODY-{}", "A".repeat(120));
        let user_msg = user(vec![text("keep-me"), text(&offloaded_body)]);
        before_model(&tp, &mc, request(vec![user_msg])).await;
        let offloaded_resp = format!("OFFLOADED-RESP-{}", "R".repeat(120));
        after_model(
            &tp,
            &mc,
            LlmResponse {
                content: Some(Content::from_text(&offloaded_resp, "model")),
                ..LlmResponse::default()
            },
        )
        .await;
        after_run(&tp, &mc).await;
        let rows = tp
            .collect_rows(until_event_type(EVENT_INVOCATION_COMPLETED))
            .await;

        let row = only_row_of_type(&rows, EVENT_LLM_REQUEST);
        let llm_content = json_object_column(&row, "content");
        let prompt_json = llm_content["prompt"].to_string();
        assert!(
            prompt_json.contains("OFFLOADED-BODY-"),
            "{name}: failed upload should retain inline text in prompt summary, got {prompt_json}"
        );
        assert_eq!(
            row.bool("is_truncated"),
            Some(true),
            "{name}: LLM_REQUEST is_truncated, want true when failed upload fell back to truncated inline text"
        );
        let resp_row = only_row_of_type(&rows, EVENT_LLM_RESPONSE);
        let resp_content = json_object_column(&resp_row, "content");
        let got = resp_content["response"].as_str().unwrap_or_default();
        assert!(
            got.starts_with("text: 'OFFLOADED-RESP-") && got.ends_with("...[TRUNCATED]'"),
            "{name}: failed LLM_RESPONSE upload should restore inline text in content.response, got {got:?}"
        );
        assert_eq!(
            resp_row.bool("is_truncated"),
            Some(true),
            "{name}: LLM_RESPONSE is_truncated"
        );
    }
    {
        let name = "batch of large offloaded texts keeps AppendRows request small";
        let off = Arc::new(HookOffloader::default());
        let tp = offloading_plugin(&off, |c| c.max_inline_text_size = 1024).await;
        let mc = Mc::of(&tp.ctx);
        let msg_text = "X".repeat(40 * 1024); // 40 KiB
        // 2 MiB total text across one LLMRequest.
        let contents = (0..50)
            .map(|_| Content::from_text(&msg_text, "user"))
            .collect();
        before_model(&tp, &mc, request(contents)).await;
        after_run(&tp, &mc).await;

        let size = serialized_record_batch_len(&first_request(&tp).await);
        // With offloaded text omitted from content, the batch is well under 200 KiB instead
        // of duplicating 2 MiB in the content column.
        assert!(
            size <= 256 * 1024,
            "{name}: serialized record batch size = {size} bytes, want <= 256 KiB when text is offloaded"
        );
    }
    {
        let name =
            "batch of 100 LLM_RESPONSE rows with 300 KiB offloaded text stays well below 20 MB";
        let off = Arc::new(HookOffloader::default());
        let tp = offloading_plugin(&off, |c| {
            c.batch_size = 100;
            c.batch_flush_intv = Duration::from_secs(3600);
            c.max_content_len = 500 * 1024;
            c.max_inline_text_size = 32 * 1024;
        })
        .await;
        let mc = Mc::of(&tp.ctx);
        let resp_text = "R".repeat(300 * 1024); // 300 KiB per response
        for _ in 0..100 {
            after_model(
                &tp,
                &mc,
                LlmResponse {
                    content: Some(Content::from_text(&resp_text, "model")),
                    ..LlmResponse::default()
                },
            )
            .await;
        }

        let size = serialized_record_batch_len(&first_request(&tp).await);
        // 100 x 300 KiB = 30 MiB if duplicated in content.response; omitting offloaded text
        // keeps the batch < 512 KiB.
        assert!(
            size <= 512 * 1024,
            "{name}: serialized LLM_RESPONSE record batch size = {size} bytes, want <= 512 KiB when 300 KiB texts are offloaded"
        );
    }
}

// parity: adapted: Go's invalid credentials file in GCSClientOptions → options whose endpoint does not parse (construction would fail if a Cloud Storage client were made)
#[tokio::test(start_paused = true)]
async fn test_gcs_offloading_skips_client_when_content_parts_disabled() {
    type Mutate = Box<dyn Fn(&mut Config)>;
    let tests: Vec<(&str, Mutate)> = vec![
        (
            "DisableMultiModalContent",
            Box::new(|c: &mut Config| c.disable_multi_modal_content = true),
        ),
        (
            "content_parts in PayloadColumnDenylist",
            Box::new(|c: &mut Config| c.payload_column_denylist = vec!["content_parts".into()]),
        ),
    ];
    for (name, mutate) in tests {
        let logger = Arc::new(RecordingLogger::default());
        let tp = new_test_plugin_with_config(|c| {
            c.gcs_bucket_name = "unused-bucket".into();
            // If newGCSOffloader were called, these options would fail construction.
            c.gcs_client_options = bad_client_options();
            c.logger = as_log(&logger);
            mutate(c);
        })
        .await;
        tp.close()
            .await
            .unwrap_or_else(|e| panic!("{name}: Close() error: {e}"));
        assert!(
            logger.contains("GCSBucketName \"unused-bucket\" is ignored"),
            "{name}: expected warning that GCSBucketName is ignored, got:\n{logger}"
        );
    }
}

// parity: adapted: Go's invalid credentials file in ClientOptions → options whose endpoint does not parse
#[tokio::test]
async fn test_gcs_offloading_gcs_client_options_overrides_client_options() {
    let fs = FakeStorage::new();
    let (gcs_opts, rt) = storage_client_options(&fs);
    let tp = new_test_plugin_with_config(|c| {
        c.gcs_bucket_name = "test-bucket".into();
        // ClientOptions would fail Cloud Storage if used; GCSClientOptions points cleanly at
        // the fake storage server.
        c.client_options = bad_client_options();
        c.gcs_client_options = gcs_opts;
    })
    .await;
    let mc = Mc::of(&tp.ctx);

    let row = log_request(&tp, &mc, request(vec![image_content(&[1, 2])])).await;
    tp.close()
        .await
        .unwrap_or_else(|e| panic!("Close() error: {e}"));

    assert_eq!(
        s(&content_parts_of(&row)[0], "storage_mode"),
        "GCS_REFERENCE",
        "storage_mode"
    );
    assert!(
        rt.recorded().iter().any(|e| e == "request POST"),
        "GCSClientOptions transport was not used: {:?}",
        rt.recorded()
    );
}

#[tokio::test(start_paused = true)]
async fn test_gcs_offloading_after_run_flush_caps_upload_deadline() {
    let off = hanging_offloader();
    let tp = offloading_plugin(&off, |c| {
        c.batch_size = 100;
        c.batch_flush_intv = Duration::from_secs(3600);
        c.shutdown_timeout = Duration::from_millis(200);
        c.gcs_upload_timeout = Duration::from_millis(900);
        c.logger = as_log(&Arc::new(RecordingLogger::default()));
    })
    .await;
    let mc = Mc::of(&tp.ctx);
    before_model(&tp, &mc, request(vec![image_content(&[1, 2, 3])])).await;

    let start = Instant::now();
    after_run(&tp, &mc).await;
    let elapsed = start.elapsed();
    // Uploads during AfterRunCallback must be capped to half of ShutdownTimeout (~100ms),
    // leaving the remaining budget for the fallback BigQuery append rather than waiting
    // GCSUploadTimeout (900ms).
    assert!(
        elapsed < Duration::from_millis(350),
        "AfterRunCallback() took {elapsed:?}, want < 350ms (upload deadline should be capped by flush deadline)"
    );

    let rows = tp
        .collect_rows(until_event_type(EVENT_INVOCATION_COMPLETED))
        .await;
    let llm_row = only_row_of_type(&rows, EVENT_LLM_REQUEST);
    assert_eq!(
        s(&content_parts_of(&llm_row)[0], "text"),
        UPLOAD_FAILED_TEXT,
        "fallback part text"
    );
}

#[tokio::test(start_paused = true)]
async fn test_gcs_offloading_run_ends_stop_waiting_once_storage_times_out() {
    let off = hanging_offloader();
    let tp = offloading_plugin(&off, |c| {
        c.batch_size = 100;
        c.batch_flush_intv = Duration::from_secs(3600);
        // The uploads of a run end get half of this, 500ms.
        c.shutdown_timeout = Duration::from_secs(1);
        c.logger = as_log(&Arc::new(RecordingLogger::default()));
    })
    .await;

    const RUNS: usize = 4;
    let mut waited = [Duration::ZERO; RUNS];
    for (i, w) in waited.iter_mut().enumerate() {
        let mc = Mc::of(&MockCtx {
            agent_name: "test-agent".into(),
            session_id: "sess-id".into(),
            invocation_id: format!("inv-{i}"),
            user_id: "user-id".into(),
            ..MockCtx::default()
        });
        before_model(&tp, &mc, request(vec![image_content(&[1, 2, 3])])).await;
        let start = Instant::now();
        after_run(&tp, &mc).await;
        *w = start.elapsed();
    }

    assert!(
        waited[0] >= Duration::from_millis(250),
        "the first run end took {:?}, want it to wait about 500ms for the uploads",
        waited[0]
    );
    for (i, d) in waited[1..].iter().enumerate() {
        assert!(
            *d < Duration::from_millis(250),
            "run end {} took {d:?}, want < 250ms: the uploads are paused once those of the first run end have timed out",
            i + 1
        );
    }
    assert_eq!(
        off.uploads().len(),
        1,
        "the offloader calls, want once, by the first run end"
    );

    let rows = tp
        .collect_rows(|rows| rows_of_type(rows, EVENT_INVOCATION_COMPLETED).len() == RUNS)
        .await;
    let llm_rows = rows_of_type(&rows, EVENT_LLM_REQUEST);
    assert_eq!(
        llm_rows.len(),
        RUNS,
        "LLM_REQUEST rows: no run end may lose rows"
    );
    for (i, row) in llm_rows.iter().enumerate() {
        assert_eq!(
            s(&content_parts_of(row)[0], "text"),
            UPLOAD_FAILED_TEXT,
            "row {i} part text"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn test_gcs_offloading_close_during_storage_stall_keeps_rows() {
    let server = FakeWriteServer::new();
    let (tx, mut rx) = requests();
    delayed_server(
        &server,
        Delayed {
            delay: Duration::from_millis(150),
            stall_after_req: 0,
            requests: Some(tx),
            received: None,
        },
    );
    let mut config = Config::default();
    (config.project_id, config.dataset_id, config.table_name) =
        ("p".into(), "d".into(), "t".into());
    config.batch_size = 10;
    config.shutdown_timeout = Duration::from_secs(2);
    config.logger = as_log(&Arc::new(RecordingLogger::default()));
    // Cloud Storage hangs: every upload waits until its context is done.
    config.gcs_offloader = as_off(&hanging_offloader());
    let p = new_bigquery_agent_analytics_plugin_with_clients(
        &Context::background(),
        config,
        Some(test_bq_client()),
        Some(server.client().unwrap()),
    )
    .await
    .unwrap();
    let mc = Mc::of(&MockCtx {
        agent_name: "a".into(),
        session_id: "s".into(),
        invocation_id: "i".into(),
        user_id: "u".into(),
        ..MockCtx::default()
    });
    let log_image = || async {
        let (_, r) = (p.before_model_callback().unwrap())(
            mc.ctx.clone(),
            request(vec![image_content(&[1, 2, 3])]),
        )
        .await;
        r.unwrap();
    };

    // The first rows go out with the next tick, whose flush then waits on Cloud Storage while
    // 85 more rows queue up behind it.
    for _ in 0..5 {
        log_image().await;
    }
    tokio::time::sleep(Duration::from_millis(1300)).await;
    for _ in 0..85 {
        log_image().await;
    }
    let close: Result<(), AgentError> = (p.close().unwrap())().await;
    close.unwrap();

    let mut written = 0;
    while let Ok(req) = rx.try_recv() {
        written += row_count(&req);
    }
    assert_eq!(
        written, 90,
        "rows written, want 90: a Cloud Storage stall must not cost BigQuery rows"
    );
}

#[tokio::test(start_paused = true)]
async fn test_gcs_offloading_failed_upload_preserves_json_prefix_in_summary() {
    let off = failing_offloader("boom");
    let tp = offloading_plugin(&off, |c| {
        c.max_inline_text_size = 1024;
        c.max_content_len = 200;
        c.logger = as_log(&Arc::new(RecordingLogger::default()));
    })
    .await;
    let mc = Mc::of(&tp.ctx);
    let msg = user(vec![text(r#"{"q":"x"}"#), text(&"y".repeat(40 * 1024))]);
    on_user_message(&tp, &mc, msg).await;
    after_run(&tp, &mc).await;

    let rows = tp
        .collect_rows(until_event_type(EVENT_INVOCATION_COMPLETED))
        .await;
    let user_row = only_row_of_type(&rows, EVENT_USER_MESSAGE_RECEIVED);
    let user_content = json_object_column(&user_row, "content");
    let summary = user_content["text_summary"].as_str().unwrap_or_default();
    assert!(
        summary.starts_with(r#"{"q":"x"} | yyy"#)
            && !summary.contains(UNPARSEABLE_JSON_PLACEHOLDER),
        "text_summary after failed upload = {summary:?}, want prefix {:?} without {UNPARSEABLE_JSON_PLACEHOLDER:?}",
        r#"{"q":"x"} | yyy"#
    );
}

#[tokio::test(start_paused = true)]
async fn test_gcs_offloading_preserves_bracket_led_prose() {
    {
        let name = "non-sensitive bracket-led prose is offloaded intact";
        let off = Arc::new(HookOffloader::default());
        let tp = offloading_plugin(&off, |c| c.max_inline_text_size = 200).await;
        let mc = Mc::of(&tp.ctx);
        let prose = format!(
            "[Chapter 1] {}",
            "ordinary prose without secrets ".repeat(20)
        );

        let row = log_request(&tp, &mc, request(vec![Content::from_text(&prose, "user")])).await;

        let uploads = off.uploads();
        assert_eq!(uploads.len(), 1, "{name}: uploads");
        assert_eq!(
            String::from_utf8_lossy(&uploads[0].data),
            prose,
            "{name}: uploaded text, want original bracket-led prose preserved"
        );
        let part = &content_parts_of(&row)[0];
        assert_eq!(
            s(part, "storage_mode"),
            "GCS_REFERENCE",
            "{name}: storage_mode"
        );
        let got = s(part, "text");
        assert!(
            got.starts_with("[Chapter 1] ") && !got.contains(UNPARSEABLE_JSON_PLACEHOLDER),
            "{name}: part text preview = {got:?}, want prefix \"[Chapter 1] \" without {UNPARSEABLE_JSON_PLACEHOLDER:?}"
        );
        assert_eq!(
            row.bool("is_truncated"),
            Some(false),
            "{name}: is_truncated"
        );
    }
    {
        let name = "bracket-led malformed JSON with credential marker still fails closed";
        let off = Arc::new(HookOffloader::default());
        let tp = offloading_plugin(&off, |c| c.max_inline_text_size = 200).await;
        let mc = Mc::of(&tp.ctx);
        let malformed = format!(r#"[{{"api_key":"sk-live-secret-999",{}"#, "x".repeat(300));

        let row = log_request(
            &tp,
            &mc,
            request(vec![Content::from_text(&malformed, "user")]),
        )
        .await;

        assert_eq!(
            s(&content_parts_of(&row)[0], "text"),
            UNPARSEABLE_JSON_PLACEHOLDER,
            "{name}: part text when credential marker is present"
        );
        assert_eq!(
            row.bool("is_truncated"),
            Some(true),
            "{name}: is_truncated, want true when malformed JSON with credential fails closed"
        );
    }
}

#[tokio::test]
async fn test_gcs_offloading_built_in_offloader_error_preserves_scrubbed_detail() {
    let fs = FakeStorage::new();
    fs.state.lock().unwrap().fail_with = 403;
    let (opts, _) = storage_client_options(&fs);
    let logger = Arc::new(RecordingLogger::default());
    let tp = new_test_plugin_with_config(|c| {
        c.gcs_bucket_name = "test-bucket".into();
        c.gcs_client_options = opts;
        c.logger = as_log(&logger);
    })
    .await;
    let mc = Mc::of(&tp.ctx);

    let row = log_request(&tp, &mc, request(vec![image_content(&[1, 2, 3])])).await;
    tp.close()
        .await
        .unwrap_or_else(|e| panic!("Close() error: {e}"));

    assert_eq!(
        s(&content_parts_of(&row)[0], "text"),
        UPLOAD_FAILED_TEXT,
        "part text"
    );
    assert!(
        logger.contains("failed to write the Cloud Storage object") && logger.contains("403"),
        "expected built-in offloader transport/API detail in log, got:\n{logger}"
    );
    assert!(
        !logger.contains("?") && !logger.contains("uploadType="),
        "built-in offloader log = {logger}, want URL query parameters stripped"
    );

    // ownedUploadError strips URI query strings and scrubs key-value credentials before
    // logging.
    let summary = safe_upload_error_summary(&OwnedUploadError::wrap(Error::msg(
        "Post \"https://storage.googleapis.com/upload/storage/v1/b/bkt/o?uploadType=multipart&access_token=test-access-tok\": api_key=sk-secret-123 x509: unknown authority",
    )));
    assert!(
        !summary.contains('?')
            && !summary.contains("uploadType")
            && !summary.contains("test-access-tok")
            && !summary.contains("sk-secret-123"),
        "safeUploadErrorSummary leaked query or secret: {summary:?}"
    );
    assert!(
        summary.contains("https://storage.googleapis.com/upload/storage/v1/b/bkt/o")
            && summary.contains("x509: unknown authority")
            && summary.contains(REDACTED_VALUE),
        "safeUploadErrorSummary = {summary:?}, want queryless URL and scrubbed diagnostic reason preserved"
    );
}
