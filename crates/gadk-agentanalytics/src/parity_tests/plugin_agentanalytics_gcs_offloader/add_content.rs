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

//! `gcs_offloader_test.go`: the tests of the plugin's row building that 7b-2 deferred to 7b-3
//! (`newAddContentPlugin` with `addContent` / `writeEvent`, `llmRequestContent`, and
//! `probeRow`'s rows built by `buildLLMRequestContent`).

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use gadk_genai::{Blob, Content, FileData, FunctionResponse, GenerateContentConfig, Part};
use gadk_model::LlmRequest;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

use super::super::plugin_agentanalytics_batch_processor::support::{
    HookOffloader, image_content, pending_upload_bytes, queue_len, utc,
};
use super::{as_log, as_off, bare, field, hooked, parts, resolve, wait_done};
use crate::batch_processor::BatchProcessor;
use crate::bigquery::fake::RecordingLogger;
use crate::config::{Config, GcsContentOffloader, Logger};
use crate::context::Context;
use crate::error::Error;
use crate::event_types::{EVENT_LLM_REQUEST, EVENT_USER_MESSAGE_RECEIVED};
use crate::formatter::{
    PartsCollector, PartsOptions, PendingUpload, TEXT_UPLOAD_CONTENT_TYPE, TRUNCATION_SUFFIX,
    UPLOAD_FAILED_TEXT,
};
use crate::plugin::rows::{build_llm_request_content, llm_request_content};
use crate::plugin::{AnalyticsPlugin, EventData, Payload};
use crate::redaction::REDACTED_VALUE;
use crate::row::{QueuedRow, Row, RowExt, RowValue, from_json_map};
use crate::trace::{CallbackCtx, Identity, RowSpan};
use crate::validate::validate_config;

// ---------------------------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------------------------

/// Go `newAddContentPlugin(t, mutate)`: a plugin that can build the content columns of a row,
/// which is all that `addContent` needs.
pub(super) fn new_add_content_plugin(mutate: impl FnOnce(&mut Config)) -> AnalyticsPlugin {
    let mut cfg = Config::default();
    mutate(&mut cfg);
    let cfg = cfg.apply_defaults();
    let denied = validate_config(&cfg).unwrap_or_else(|e| panic!("validateConfig() error: {e}"));
    AnalyticsPlugin {
        config: cfg,
        denied_columns: denied,
        ..AnalyticsPlugin::default()
    }
}

/// Go `p.addContent(row, eventType, &eventData{content: payload})`: the row and the result.
fn add_content(p: &AnalyticsPlugin, event_type: &str, payload: Payload) -> (QueuedRow, bool) {
    let mut row = QueuedRow::default();
    let mut d = EventData {
        content: Some(payload),
        ..EventData::default()
    };
    let truncated = p.add_content(&mut row, event_type, &mut d);
    (row, truncated)
}

/// Go `&model.LLMRequest{Contents: contents}`.
fn request(contents: Vec<Content>) -> Payload {
    Payload::LlmRequest(Box::new(LlmRequest {
        contents,
        ..LlmRequest::default()
    }))
}

/// Go `&genai.Content{Role: "user", Parts: parts}`.
fn user(parts: Vec<Part>) -> Content {
    Content {
        role: "user".to_owned(),
        parts,
    }
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

/// Go `{FunctionResponse: &genai.FunctionResponse{Name: name, Response: response}}`.
fn function_response(name: &str, response: Value) -> Part {
    let Value::Object(response) = response else {
        panic!("a response is a JSON object");
    };
    Part {
        function_response: Some(FunctionResponse {
            name: name.to_owned(),
            response,
            ..FunctionResponse::default()
        }),
        ..Part::default()
    }
}

/// Go `&BatchProcessor{config: config, queue: make(chan map[string]any, capacity)}`.
fn processor_with(config: Config, capacity: usize) -> Arc<BatchProcessor> {
    Arc::new(BatchProcessor::bare(config, capacity))
}

/// `off` as a fresh `&fakeOffloader{}`.
fn fake_offloader() -> Option<Arc<dyn GcsContentOffloader>> {
    as_off(&Arc::new(HookOffloader::default()))
}

/// Go `probeRow(t, contents...)`.
pub(super) fn probe_row(contents: Vec<Content>) -> QueuedRow {
    probe_row_with_options(
        PartsOptions {
            max_len: 50,
            offload: true,
            inline_limit: 10,
            ..PartsOptions::default()
        },
        contents,
    )
}

/// Go `probeRowWithOptions(t, opts, contents...)`: the row of an LLM request whose content is
/// built by `buildLLMRequestContent`, with the parts to upload and the summary builder that
/// the plugin queues with it.
pub(crate) fn probe_row_with_options(opts: PartsOptions, contents: Vec<Content>) -> QueuedRow {
    let max_len = opts.max_len;
    let mut parts = PartsCollector::new(opts);
    let req = LlmRequest {
        contents,
        ..LlmRequest::default()
    };
    let (content, content_truncated, builder) =
        build_llm_request_content(&req, max_len, false, false, Some(&mut parts));
    let b = serde_json::to_string(&content).unwrap_or_else(|e| panic!("{e}"));
    let mut row = Row::new();
    row.insert(
        "timestamp".to_owned(),
        RowValue::Timestamp(utc(2026, 3, 4, 5, 6, 7, 0)),
    );
    row.insert("event_id".to_owned(), RowValue::from("event1"));
    row.insert("trace_id".to_owned(), RowValue::from("trace1"));
    row.insert("span_id".to_owned(), RowValue::from("span1"));
    row.insert("content".to_owned(), RowValue::Str(b));
    row.insert(
        "content_parts".to_owned(),
        RowValue::Parts(parts.parts.iter().map(from_json_map).collect()),
    );
    row.insert(
        "is_truncated".to_owned(),
        RowValue::Bool(content_truncated || parts.truncated),
    );
    let mut q = QueuedRow::new(row);
    if !parts.pending.is_empty() {
        if let Some(mut builder) = builder
            && parts.has_pending_text()
        {
            builder.other_truncated = parts.non_upload_truncated;
            q.summary_builder = Some(Arc::new(Mutex::new(builder)));
        }
        q.raw_parts = std::mem::take(&mut parts.pending);
    }
    q
}

/// The fakeOffloader of the probe-row tests: a call waits for its context while `stalled`,
/// and fails when `fail_text` and the call is the upload of a text part.
fn stall_when(stalled: &Arc<AtomicBool>, fail_text: bool) -> Arc<HookOffloader> {
    let stalled = Arc::clone(stalled);
    hooked(move |ctx, req| {
        let stalled = stalled.load(Ordering::SeqCst);
        Box::pin(async move {
            if fail_text && req.content_type == TEXT_UPLOAD_CONTENT_TYPE {
                return Err(Error::msg("permanent text upload failure"));
            }
            if stalled {
                return wait_done(ctx).await;
            }
            Ok(())
        })
    })
}

/// Go `&BatchProcessor{config: Config{GCSOffloader: off, GCSUploadTimeout: time.Minute,
/// Logger: &recordingLogger{}}}`.
fn probe_processor(off: &Arc<HookOffloader>) -> Arc<BatchProcessor> {
    bare(Config {
        gcs_offloader: as_off(off),
        gcs_upload_timeout: Duration::from_secs(60),
        logger: as_log(&Arc::new(RecordingLogger::default())),
        ..Config::zero()
    })
}

/// The control / probe sequence of the deferred-retry tests: `control` uploads in one pass;
/// `probe`'s uploads are cut short by a 30ms caller deadline while storage stalls, requeued,
/// and retried once storage is healthy again.
async fn control_and_deferred_probe(
    stalled: &Arc<AtomicBool>,
    bp: &Arc<BatchProcessor>,
    mk: impl Fn() -> QueuedRow,
) -> (QueuedRow, QueuedRow) {
    let mut control = mk();
    resolve(
        bp,
        &Context::background(),
        false,
        std::slice::from_mut(&mut control),
    )
    .await;

    let mut probe = mk();
    stalled.store(true, Ordering::SeqCst);
    let ctx = Context::background().with_timeout(Duration::from_millis(30));
    resolve(bp, &ctx, false, std::slice::from_mut(&mut probe)).await;
    ctx.cancel();
    bp.requeue_deferred_uploads(std::slice::from_mut(&mut probe));
    assert!(
        !probe.raw_parts.is_empty(),
        "probe row was not requeued for upload"
    );
    stalled.store(false, Ordering::SeqCst);
    resolve(
        bp,
        &Context::background(),
        false,
        std::slice::from_mut(&mut probe),
    )
    .await;
    (control, probe)
}

// ---------------------------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------------------------

// parity: adapted: Rust's addContent borrows the message, so "the caller goes on using its message" is the caller changing its EventData after the call (a Bytes blob is replaced, not written in place)
#[test]
fn test_add_content_queued_payloads_are_copies() {
    let p = new_add_content_plugin(|c| {
        c.gcs_offloader = fake_offloader();
        c.max_inline_text_size = 10;
    });
    let long_text = "x".repeat(50);
    let mut row = QueuedRow::default();
    let mut d = EventData {
        content: Some(Payload::Content(user(vec![
            text(&long_text),
            inline("image/png", &[1, 2, 3]),
        ]))),
        ..EventData::default()
    };

    p.add_content(&mut row, EVENT_USER_MESSAGE_RECEIVED, &mut d);
    // The caller goes on using its message.
    if let Some(Payload::Content(c)) = d.content.as_mut() {
        c.parts[0].text = "changed".to_owned();
        let blob = c.parts[1].inline_data.as_mut().unwrap();
        blob.data = Bytes::from_static(&[0xEE, 2, 3]);
        blob.mime_type = "image/gif".to_owned();
    }

    let want = vec![
        PendingUpload {
            slot: 0,
            content_ord: 0,
            part_idx: 0,
            mime_type: "text/plain".into(),
            is_text: true,
            text: long_text.clone(),
            ..PendingUpload::default()
        },
        PendingUpload {
            slot: 1,
            content_ord: 0,
            part_idx: 1,
            mime_type: "image/png".into(),
            data: Bytes::from_static(&[1, 2, 3]),
            ..PendingUpload::default()
        },
    ];
    assert_eq!(row.raw_parts, want, "queued uploads mismatch");
    let got = parts(&row);
    assert!(
        got.len() == 2
            && field(&got[0], "text") == long_text
            && field(&got[1], "mime_type") == "image/png",
        "content_parts = {got:?}, want the entries of the message as it was"
    );
}

#[test]
fn test_add_content_queues_only_when_offloading() {
    let long = user(vec![text(&"x".repeat(50)), inline("image/png", &[1])]);
    let short = Content::from_text("short", "user");
    let offload = |c: &mut Config| {
        c.gcs_offloader = fake_offloader();
        c.max_inline_text_size = 10;
    };
    type Mutate = Box<dyn Fn(&mut Config)>;
    // (name, mutate, content, wantParts, wantQueued, wantContent): wantParts and wantQueued
    // say whether the row gets content_parts and queued uploads; wantContent whether it gets
    // the content column.
    let tests: Vec<(&str, Mutate, &Content, bool, bool, bool)> = vec![
        (
            "with an offloader",
            Box::new(offload),
            &long,
            true,
            true,
            true,
        ),
        (
            "without an offloader",
            Box::new(|c: &mut Config| c.max_inline_text_size = 10),
            &long,
            true,
            false,
            true,
        ),
        (
            "text within the limit",
            Box::new(offload),
            &short,
            true,
            false,
            true,
        ),
        (
            "content_parts denied",
            Box::new(move |c: &mut Config| {
                offload(c);
                c.payload_column_denylist = vec!["content_parts".into()];
            }),
            &long,
            false,
            false,
            true,
        ),
        (
            // The upload is about content_parts, not about content.
            "content denied",
            Box::new(move |c: &mut Config| {
                offload(c);
                c.payload_column_denylist = vec!["content".into()];
            }),
            &long,
            true,
            true,
            false,
        ),
        (
            "multi-modal content disabled",
            Box::new(move |c: &mut Config| {
                offload(c);
                c.disable_multi_modal_content = true;
            }),
            &long,
            false,
            false,
            true,
        ),
    ];
    for (name, mutate, content, want_parts, want_queued, want_content) in tests {
        let p = new_add_content_plugin(|c| mutate(c));

        let (row, _) = add_content(
            &p,
            EVENT_USER_MESSAGE_RECEIVED,
            Payload::Content(content.clone()),
        );

        assert_eq!(
            row.row.contains_key("content_parts"),
            want_parts,
            "{name}: row has content_parts"
        );
        assert_eq!(
            !row.raw_parts.is_empty(),
            want_queued,
            "{name}: row has queued uploads"
        );
        assert_eq!(
            row.row.contains_key("content"),
            want_content,
            "{name}: row has content"
        );
    }
}

// parity: adapted: a Rust request cannot hold Go's nil *Content, so the second message is an empty Content (no parts; it keeps its number as Go's nil message does)
#[test]
fn test_llm_request_content_content_ordinals() {
    let req = LlmRequest {
        contents: vec![image_content(&[1]), Content::default(), image_content(&[3])],
        config: Some(GenerateContentConfig {
            system_instruction: Some(image_content(&[4])),
            ..GenerateContentConfig::default()
        }),
        ..LlmRequest::default()
    };
    let mut c = PartsCollector::new(PartsOptions {
        max_len: 1000,
        redact: true,
        offload: true,
        inline_limit: 50,
        ..PartsOptions::default()
    });

    llm_request_content(&req, 1000, true, Some(&mut c));

    // The nil message keeps its number, and the system instruction comes after all of the
    // messages. Slots count the entries of content_parts.
    let png = |slot: usize, content_ord: usize, data: &'static [u8]| PendingUpload {
        slot,
        content_ord,
        part_idx: 0,
        mime_type: "image/png".into(),
        data: Bytes::from_static(data),
        ..PendingUpload::default()
    };
    let want = vec![png(0, 0, &[1]), png(1, 2, &[3]), png(2, 3, &[4])];
    assert_eq!(c.pending, want, "queued uploads mismatch");
}

#[test]
fn test_llm_request_content_redaction() {
    let req = LlmRequest {
        contents: vec![Content::from_text("call it with api_key=SECRET123", "user")],
        config: Some(GenerateContentConfig {
            system_instruction: Some(Content {
                parts: vec![text("db password: hunter2")],
                ..Content::default()
            }),
            ..GenerateContentConfig::default()
        }),
        ..LlmRequest::default()
    };
    // (name, redact, parts, wantSecret)
    let tests = [
        ("redacted without parts", true, None, false),
        (
            "redacted with parts",
            true,
            Some(PartsOptions {
                max_len: 1000,
                redact: true,
                ..PartsOptions::default()
            }),
            false,
        ),
        ("not redacted without parts", false, None, true),
    ];
    for (name, redact, parts_opts, want_secret) in tests {
        let mut parts = parts_opts.map(PartsCollector::new);
        let (content, _) = llm_request_content(&req, 1000, redact, parts.as_mut());
        let got = serde_json::to_string(&content)
            .unwrap_or_else(|e| panic!("{name}: json.Marshal({content:?}) error: {e}"));
        for secret in ["SECRET123", "hunter2"] {
            assert_eq!(
                got.contains(secret),
                want_secret,
                "{name}: content {got} contains {secret:?}"
            );
        }
        if !want_secret {
            assert!(
                got.contains(REDACTED_VALUE),
                "{name}: content {got} has no {REDACTED_VALUE:?}"
            );
        }
    }
}

#[test]
fn test_add_content_part_attributes_follow_the_redaction_setting() {
    const SECRET: &str = "sk-test-secret-value";
    type Mutate = Box<dyn Fn(&mut Config)>;
    let tests: Vec<(&str, Mutate, &str)> = vec![
        ("by default", Box::new(|_: &mut Config| {}), REDACTED_VALUE),
        (
            "redaction disabled",
            Box::new(|c: &mut Config| c.disable_sensitive_key_redaction = true),
            SECRET,
        ),
    ];
    for (name, mutate, want_api_key) in tests {
        let p = new_add_content_plugin(|c| mutate(c));
        let content = user(vec![function_response(
            "lookup",
            json!({"api_key": SECRET, "result": "ok"}),
        )]);

        let (row, _) = add_content(&p, EVENT_USER_MESSAGE_RECEIVED, Payload::Content(content));

        let got = parts(&row);
        assert_eq!(
            got.len(),
            1,
            "{name}: content_parts = {got:?}, want one entry"
        );
        let attrs: Value =
            serde_json::from_str(field(&got[0], "part_attributes")).unwrap_or_else(|e| {
                panic!(
                    "{name}: part_attributes {:?} is not JSON: {e}",
                    got[0].get("part_attributes")
                )
            });
        assert_eq!(
            attrs["function_response"]["response"],
            json!({"api_key": want_api_key, "result": "ok"}),
            "{name}: the response in part_attributes mismatch"
        );
    }
}

#[test]
fn test_add_content_truncated_parts_mark_the_row() {
    let tests = [
        (
            "a function response cut to the limit",
            function_response("lookup", json!({"data": "x".repeat(1000)})),
            true,
        ),
        (
            "a file whose URI carried credentials",
            Part {
                file_data: Some(FileData {
                    mime_type: "image/png".into(),
                    file_uri: "https://user:pass@example.com/a.png".into(),
                    ..FileData::default()
                }),
                ..Part::default()
            },
            true,
        ),
        (
            "a function response within the limit",
            function_response("lookup", json!({"data": "x"})),
            false,
        ),
    ];
    for (name, part, want) in tests {
        let p = new_add_content_plugin(|c| c.max_content_len = 100);

        let (row, got) = add_content(
            &p,
            EVENT_USER_MESSAGE_RECEIVED,
            Payload::Content(user(vec![part])),
        );

        assert_eq!(got, want, "{name}: addContent()");
        // The content column is not what truncated the row.
        let content = row.row.str("content").unwrap_or_default();
        assert!(
            !content.contains(TRUNCATION_SUFFIX),
            "{name}: the content column was truncated: {content}"
        );
    }
}

#[test]
fn test_add_content_default_inline_limit_is32_ki_b() {
    let tests = [
        ("at the limit", 32 * 1024, false),
        ("one byte over the limit", 32 * 1024 + 1, true),
    ];
    for (name, size, want_queued) in tests {
        let p = new_add_content_plugin(|c| {
            c.gcs_offloader = fake_offloader();
            c.max_inline_text_size = 0;
        });

        let (row, _) = add_content(
            &p,
            EVENT_USER_MESSAGE_RECEIVED,
            Payload::Content(Content::from_text("x".repeat(size), "user")),
        );

        assert_eq!(
            !row.raw_parts.is_empty(),
            want_queued,
            "{name}: a text of {size} bytes is queued for upload"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn test_gcs_offloading_pending_upload_bytes_cap() {
    let logger = Arc::new(RecordingLogger::default());
    let off = fake_offloader();
    let bp = processor_with(
        Config {
            gcs_offloader: off.clone(),
            batch_size: 100,
            logger: as_log(&logger),
            ..Config::zero()
        },
        2,
    );
    bp.max_pending_upload_bytes.store(100, Ordering::SeqCst);
    let mut p = new_add_content_plugin(|c| {
        c.gcs_offloader = off.clone();
        c.max_inline_text_size = 20;
    });
    p.processor = Some(Arc::clone(&bp));
    let image = |n: usize| request(vec![image_content(&vec![0; n])]);

    // First 60-byte image fits under the 100-byte cap.
    let (row1, _) = add_content(&p, EVENT_LLM_REQUEST, image(60));
    assert!(
        !row1.raw_parts.is_empty(),
        "row1 should be queued for upload (60 <= 100)"
    );
    assert_eq!(
        pending_upload_bytes(&bp),
        60,
        "pendingUploadBytes after row1"
    );
    bp.append(row1);

    // Second 60-byte image exceeds the 100-byte cap (60 + 60 > 100): not queued, marked
    // [UPLOAD FAILED].
    let (row2, _) = add_content(&p, EVENT_LLM_REQUEST, image(60));
    assert!(
        row2.raw_parts.is_empty(),
        "row2 should not be queued when pending upload bytes cap is exceeded"
    );
    assert_eq!(
        field(&parts(&row2)[0], "text"),
        UPLOAD_FAILED_TEXT,
        "row2 part text when budget is exceeded"
    );
    assert!(
        logger.contains("pending GCS upload byte budget (100 bytes) exceeded"),
        "expected budget exhaustion warning in log, got:\n{logger}"
    );
    assert_eq!(
        pending_upload_bytes(&bp),
        60,
        "pendingUploadBytes after rejected row2"
    );

    // Resolving row1 releases its 60 reserved bytes.
    let mut drained = bp.drain_queue();
    resolve(&bp, &Context::background(), false, &mut drained).await;
    assert_eq!(
        pending_upload_bytes(&bp),
        0,
        "pendingUploadBytes after resolveUploads"
    );

    // If a row with reserved bytes is dropped because the queue is full, its reservation is
    // released.
    bp.append(QueuedRow::default());
    bp.append(QueuedRow::default());
    assert_eq!(queue_len(&bp), 2, "len(bp.queue), want full capacity");
    let (overflow_row, _) = add_content(&p, EVENT_LLM_REQUEST, image(40));
    assert_eq!(
        pending_upload_bytes(&bp),
        40,
        "pendingUploadBytes before full-queue Append"
    );
    bp.append(overflow_row);
    assert_eq!(
        pending_upload_bytes(&bp),
        0,
        "pendingUploadBytes after full-queue dropped Append"
    );
    let _ = bp.drain_queue();

    // If a row with reserved bytes is dropped because the processor is closed, its
    // reservation is released.
    bp.closed.store(true, Ordering::SeqCst);
    let (row3, _) = add_content(&p, EVENT_LLM_REQUEST, image(40));
    assert_eq!(
        pending_upload_bytes(&bp),
        40,
        "pendingUploadBytes before dropped Append"
    );
    bp.append(row3);
    assert_eq!(
        pending_upload_bytes(&bp),
        0,
        "pendingUploadBytes after dropped Append"
    );
}

#[test]
fn test_gcs_offloading_configurable_pending_upload_bytes_cap() {
    let off = fake_offloader();
    let bp = processor_with(
        Config {
            gcs_offloader: off.clone(),
            gcs_max_pending_upload_bytes: 80,
            logger: as_log(&Arc::new(RecordingLogger::default())),
            ..Config::zero()
        }
        .apply_defaults(),
        4,
    );
    let mut p = new_add_content_plugin(|c| {
        c.gcs_offloader = off.clone();
        c.gcs_max_pending_upload_bytes = 80;
    });
    p.processor = Some(Arc::clone(&bp));
    let image = |n: usize| request(vec![image_content(&vec![0; n])]);

    let (row_too_large, _) = add_content(&p, EVENT_LLM_REQUEST, image(100));
    assert!(
        row_too_large.raw_parts.is_empty(),
        "100-byte payload should be rejected when GCSMaxPendingUploadBytes is 80"
    );

    let (row_fits, _) = add_content(&p, EVENT_LLM_REQUEST, image(60));
    assert!(
        !row_fits.raw_parts.is_empty(),
        "60-byte payload should be reserved when GCSMaxPendingUploadBytes is 80"
    );
    assert_eq!(pending_upload_bytes(&bp), 60, "pendingUploadBytes");
}

/// Go `panicOnBudgetLogger`: a recording logger whose `Printf` panics on the budget warning.
#[derive(Default)]
struct PanicOnBudgetLogger(RecordingLogger);

impl Logger for PanicOnBudgetLogger {
    fn printf(&self, args: fmt::Arguments<'_>) {
        if args.to_string().contains("upload byte budget") {
            panic!("logger panic after first part reserved bytes");
        }
        self.0.printf(args);
    }
    fn println(&self, args: fmt::Arguments<'_>) {
        self.0.println(args);
    }
}

#[test]
fn test_gcs_offloading_build_row_panic_releases_reserved_bytes() {
    let logger = Arc::new(PanicOnBudgetLogger::default());
    let off = fake_offloader();
    let bp = processor_with(
        Config {
            gcs_offloader: off.clone(),
            gcs_max_pending_upload_bytes: 100,
            logger: as_log(&logger),
            ..Config::zero()
        }
        .apply_defaults(),
        4,
    );
    let mut p = new_add_content_plugin(|c| {
        c.gcs_offloader = off.clone();
        c.gcs_max_pending_upload_bytes = 100;
        c.logger = as_log(&logger);
    });
    p.processor = Some(Arc::clone(&bp));

    // Part 0 reserves 60 bytes (<= 100). Part 1 (60 bytes) exceeds the 100-byte cap and logs
    // a budget warning whose Logger.Printf panics before Append; the reserved part travels
    // with the row, so buildRow's guard releases it.
    p.write_event(
        &CallbackCtx::background(),
        &Identity::default(),
        EVENT_LLM_REQUEST,
        RowSpan::default(),
        Some(&mut |d: &mut EventData| {
            d.content = Some(request(vec![user(vec![
                inline("image/png", &[0; 60]),
                inline("image/png", &[0; 60]),
            ])]));
        }),
    );

    assert_eq!(
        pending_upload_bytes(&bp),
        0,
        "pendingUploadBytes after buildRow panic (reservation must be released)"
    );
}

#[tokio::test(start_paused = true)]
async fn test_gcs_offloading_deferred_text_retry_truncation_flag() {
    let stalled = Arc::new(AtomicBool::new(false));
    let off = stall_when(&stalled, false);
    let bp = probe_processor(&off);
    let long = "x".repeat(200);

    let (control, probe) = control_and_deferred_probe(&stalled, &bp, || {
        probe_row(vec![Content::from_text(&long, "user")])
    })
    .await;

    assert_eq!(
        probe.row.get("is_truncated"),
        control.row.get("is_truncated"),
        "is_truncated after deferred retry (left) vs one-pass success (right)"
    );
    assert_eq!(
        probe.row.get("content"),
        control.row.get("content"),
        "content after deferred retry differs"
    );
}

#[tokio::test(start_paused = true)]
async fn test_gcs_offloading_binary_only_deferral_keeps_failed_text_summary() {
    let stalled = Arc::new(AtomicBool::new(false));
    let off = stall_when(&stalled, true);
    let bp = probe_processor(&off);
    let long = "y".repeat(200);

    let (control, probe) = control_and_deferred_probe(&stalled, &bp, || {
        probe_row(vec![user(vec![
            text(&long),
            inline("image/png", &[1, 2, 3]),
        ])])
    })
    .await;

    assert_eq!(
        probe.row.get("is_truncated"),
        control.row.get("is_truncated"),
        "is_truncated after deferred retry (left) vs one-pass (right)"
    );
    assert_eq!(
        probe.row.get("content"),
        control.row.get("content"),
        "content after deferred retry differs"
    );
}

#[tokio::test(start_paused = true)]
async fn test_gcs_offloading_failed_multi_attribute_upload_truncated_flag() {
    let off = hooked(|_, _| Box::pin(async { Err(Error::msg("gcs failure")) }));
    let bp = probe_processor(&off);
    let long = "z".repeat(200);
    let content = user(vec![Part {
        text: long,
        inline_data: Some(Blob {
            mime_type: "image/png".into(),
            data: Bytes::from_static(&[1, 2, 3]),
            ..Blob::default()
        }),
        ..Part::default()
    }]);
    let mut row = probe_row(vec![content]);

    resolve(
        &bp,
        &Context::background(),
        false,
        std::slice::from_mut(&mut row),
    )
    .await;

    assert_eq!(
        row.row.bool("is_truncated"),
        Some(true),
        "is_truncated after failed text upload with multi-attribute part"
    );
}
