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

//! Port of ADK-Go `plugin/agentanalytics/gcs_offloader_test.go` (ledger `docs/parity/7b-2.md`).
//! Go's `fakeOffloader`, `imageContent(s)` and `queuedRow` are the batch-processor ports'
//! `support` items; Go's `&BatchProcessor{config: …}` literals are `BatchProcessor::bare`;
//! Go's `fakeStorage` is a `FakeServer` handler; Go's gRPC servers are hooks on the in-process
//! `FakeWriteServer`; time is tokio's paused clock (Go's sleeps, timeouts and `synctest` bubbles
//! advance it), except for the four `newGCSOffloader` tests, which talk HTTP to a local server.
//! Go's `panicValue(f)` is [`panic_value`] (a future whose panic is caught) or
//! [`panic_value_sync`]. The tests are split by the Go file's order: this file (the offloader,
//! object paths and the first `resolveUploads` tests), [`helpers`], [`deadlines`],
//! [`deferred`]; the tests that need the plugin (ported in 7b-3a) are in [`add_content`]
//! (`addContent`, `writeEvent`, `llmRequestContent`, `probeRow`) and [`plugin_level`] (the
//! plugin's constructors, callbacks and Close).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::field_reassign_with_default
)] // Go's `cfg := DefaultConfig(); cfg.X = …` reads as written

pub(super) mod add_content;
mod deadlines;
mod deferred;
mod helpers;
pub(super) mod plugin_level;

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use chrono::{FixedOffset, TimeZone, Utc};
use gadk_genai::{Blob, Content, Part};
use gadk_model::rest::test_util::{FakeResponse, FakeServer, RecordedRequest};
use gadk_platform::BoxFuture;
use gadk_platform::env::MapEnv;
use pretty_assertions::assert_eq;

use super::plugin_agentanalytics_batch_processor::support::{
    HookOffloader, OffloadHook, UploadAttempt, UriFn, image_content, queued_row, utc,
};
use crate::batch_processor::BatchProcessor;
use crate::bigquery::fake::RecordingLogger;
use crate::config::{ClientOptions, Config, GcsContentOffloader, GcsUploadRequest, Logger};
use crate::context::{Context, ContextError};
use crate::error::{ApiError, Error};
use crate::formatter::{PartsOptions, PendingUpload, UPLOAD_FAILED_TEXT};
use crate::offload::gcs::{GcsOffloader, is_precondition_failed};
use crate::offload::uploads::{UploadResolveOptions, catch_unwind_future, gcs_object_path};
use crate::row::{QueuedRow, Row, RowExt, RowValue};

// ---------------------------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------------------------

/// Go `bp.resolveUploads(ctx, pauseOnCallerDeadline, batch)`: `resolve_uploads_for_batch`
/// with `can_defer`.
pub(super) async fn resolve(
    bp: &BatchProcessor,
    ctx: &Context,
    pause_on_caller_deadline: bool,
    batch: &mut [QueuedRow],
) {
    bp.resolve_uploads_for_batch(
        ctx,
        UploadResolveOptions {
            pause_on_caller_deadline,
            can_defer: true,
        },
        batch,
    )
    .await;
}

/// Go `&BatchProcessor{config: config, queue: make(chan map[string]any, 4)}`.
pub(super) fn bare(config: Config) -> Arc<BatchProcessor> {
    Arc::new(BatchProcessor::bare(config, 4))
}

/// Go `partsOptions{maxLen: 1000, offload: true, inlineLimit: inline_limit}`.
pub(super) fn opts(inline_limit: i64) -> PartsOptions {
    PartsOptions {
        max_len: 1000,
        offload: true,
        inline_limit,
        ..PartsOptions::default()
    }
}

/// [`opts`] with `reserveBytes: bp.tryReserveUploadBytes`.
pub(super) fn reserving(bp: &Arc<BatchProcessor>, inline_limit: i64) -> PartsOptions {
    let bp = Arc::clone(bp);
    PartsOptions {
        reserve_bytes: Some(Arc::new(move |n| bp.try_reserve_upload_bytes(n))),
        ..opts(inline_limit)
    }
}

/// A `fakeOffloader.hook`.
pub(super) fn hook(
    f: impl Fn(Context, GcsUploadRequest) -> BoxFuture<'static, Result<(), Error>>
    + Send
    + Sync
    + 'static,
) -> OffloadHook {
    Arc::new(f)
}

/// Go `&fakeOffloader{hook: f}`.
pub(super) fn hooked(
    f: impl Fn(Context, GcsUploadRequest) -> BoxFuture<'static, Result<(), Error>>
    + Send
    + Sync
    + 'static,
) -> Arc<HookOffloader> {
    Arc::new(HookOffloader::with_hook(hook(f)))
}

/// `off` as a config offloader.
pub(super) fn as_off(off: &Arc<HookOffloader>) -> Option<Arc<dyn GcsContentOffloader>> {
    Some(Arc::clone(off) as Arc<dyn GcsContentOffloader>)
}

/// `logger` as a config logger.
pub(super) fn as_log<L: Logger + 'static>(logger: &Arc<L>) -> Option<Arc<dyn Logger>> {
    Some(Arc::clone(logger) as Arc<dyn Logger>)
}

/// Go `ctx.Err()` as an error.
pub(super) fn ctx_err(ctx: &Context) -> Error {
    Error::Context(ctx.err().unwrap_or(ContextError::Canceled))
}

/// Go `<-ctx.Done(); return ctx.Err()`.
pub(super) async fn wait_done(ctx: Context) -> Result<(), Error> {
    ctx.done().await;
    Err(ctx_err(&ctx))
}

/// Go `stallingOffloader(stalled)`.
pub(super) fn stalling_offloader(stalled: &Arc<AtomicBool>) -> Arc<HookOffloader> {
    let stalled = Arc::clone(stalled);
    hooked(move |ctx, _| {
        let stalled = stalled.load(Ordering::SeqCst);
        Box::pin(async move {
            if stalled {
                return wait_done(ctx).await;
            }
            Ok(())
        })
    })
}

/// `row` with `event_id` set.
pub(super) fn with_event_id(mut q: QueuedRow, id: &str) -> QueuedRow {
    q.row.insert("event_id".to_owned(), RowValue::from(id));
    q
}

/// `row` with `event_type` (and an `event_id` when `id` is not empty) set.
pub(super) fn tagged(mut q: QueuedRow, event_type: &str, id: &str) -> QueuedRow {
    q.row
        .insert("event_type".to_owned(), RowValue::from(event_type));
    if id.is_empty() {
        q
    } else {
        with_event_id(q, id)
    }
}

/// Go `uploadRows(bp, n)`: rows `ev-0` … that each queue a 16-byte image and reserve it.
pub(super) fn upload_rows(bp: &Arc<BatchProcessor>, n: usize) -> Vec<QueuedRow> {
    let o = reserving(bp, 10);
    (0..n)
        .map(|i| {
            with_event_id(
                queued_row(&o, &[image_content(&[0; 16])]),
                &format!("ev-{i}"),
            )
        })
        .collect()
}

/// Go `deferUploads(t, bp, batch)`: resolves `batch` under a 25ms caller deadline while the
/// uploads stall; every row must keep a deferred upload and nothing may be logged (the logger
/// may panic on it).
pub(super) async fn defer_uploads(bp: &BatchProcessor, batch: &mut [QueuedRow]) {
    let ctx = Context::background().with_timeout(Duration::from_millis(25));
    if let Some(p) = panic_value(resolve(bp, &ctx, false, batch)).await {
        panic!(
            "resolveUploads logged the failure of a deferred upload: the logger panicked with {p}"
        );
    }
    ctx.cancel();
    for (i, q) in batch.iter().enumerate() {
        assert!(
            q.deferred.as_ref().is_some_and(|d| !d.pending.is_empty()),
            "row {i} has no deferred upload"
        );
    }
}

/// Go `uploadFailureLines(l)`.
pub(super) fn upload_failure_lines(logger: &RecordingLogger) -> Vec<String> {
    logger
        .lines()
        .into_iter()
        .filter(|l| l.contains("GCS upload failed"))
        .collect()
}

/// A panic payload as text.
fn panic_text(p: &(dyn std::any::Any + Send)) -> String {
    p.downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| p.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "<non-string panic>".to_owned())
}

/// Go `panicValue(f)` for a future: the text it panicked with, or `None`.
pub(super) async fn panic_value<F: Future>(f: F) -> Option<String> {
    catch_unwind_future(f)
        .await
        .err()
        .map(|p| panic_text(p.as_ref()))
}

/// Go `panicValue(f)`.
pub(super) fn panic_value_sync(f: impl FnOnce()) -> Option<String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f))
        .err()
        .map(|p| panic_text(p.as_ref()))
}

/// Go `reproPanicLogger`: panics on the lines of failed uploads; logs nothing else.
#[derive(Default)]
pub(super) struct ReproPanicLogger;

impl Logger for ReproPanicLogger {
    fn printf(&self, args: std::fmt::Arguments<'_>) {
        if args.to_string().contains("GCS upload failed") {
            panic!("logger boom");
        }
    }

    fn println(&self, _args: std::fmt::Arguments<'_>) {}
}

/// Go `alwaysPanicLogger`: panics on every line, with the line in the panic.
#[derive(Default)]
pub(super) struct AlwaysPanicLogger;

impl Logger for AlwaysPanicLogger {
    fn printf(&self, args: std::fmt::Arguments<'_>) {
        panic!("logger boom: {args}");
    }

    fn println(&self, _args: std::fmt::Arguments<'_>) {
        panic!("logger boom");
    }
}

/// A logger that records like Go's `recordingLogger` until `panicking` is set, then behaves as
/// `P` (Go tests that assign `bp.config.Logger` after construction; gadk's config is fixed at
/// construction).
pub(super) struct SwitchLogger<P: Logger> {
    pub(super) inner: Arc<RecordingLogger>,
    pub(super) panicking: AtomicBool,
    pub(super) panicker: P,
}

impl<P: Logger + Default> SwitchLogger<P> {
    pub(super) fn new(panicking: bool) -> Arc<SwitchLogger<P>> {
        Arc::new(SwitchLogger {
            inner: Arc::new(RecordingLogger::default()),
            panicking: AtomicBool::new(panicking),
            panicker: P::default(),
        })
    }
}

impl<P: Logger> Logger for SwitchLogger<P> {
    fn printf(&self, args: std::fmt::Arguments<'_>) {
        if self.panicking.load(Ordering::SeqCst) {
            self.panicker.printf(args);
        } else {
            self.inner.printf(args);
        }
    }

    fn println(&self, args: std::fmt::Arguments<'_>) {
        if self.panicking.load(Ordering::SeqCst) {
            self.panicker.println(args);
        } else {
            self.inner.println(args);
        }
    }
}

/// The `content_parts` of a queued row (Go `row[columnContentParts].([]map[string]any)`).
pub(super) fn parts(q: &QueuedRow) -> Vec<Row> {
    q.row
        .parts("content_parts")
        .map(<[Row]>::to_vec)
        .unwrap_or_default()
}

/// A part's string field (Go `part[key]` compared with a string).
pub(super) fn field<'a>(part: &'a Row, key: &str) -> &'a str {
    part.str(key).unwrap_or_default()
}

/// How many uploads `q` keeps for a retry (Go `len(row[deferredUploadsKey].pending)`).
pub(super) fn kept(q: &QueuedRow) -> usize {
    q.deferred.as_ref().map_or(0, |d| d.pending.len())
}

// ---------------------------------------------------------------------------------------------
// fakeStorage (the Cloud Storage JSON API)
// ---------------------------------------------------------------------------------------------

/// Go `storedObject`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct StoredObject {
    content_type: String,
    data: Bytes,
}

#[derive(Default)]
struct StorageState {
    objects: BTreeMap<String, StoredObject>,
    queries: Vec<BTreeMap<String, String>>,
    fail_with: u16,
}

/// Go `fakeStorage`: handles the multipart uploads of the storage client and keeps the objects.
struct FakeStorage {
    server: FakeServer,
    state: Arc<Mutex<StorageState>>,
}

/// Go `writeStorageError(w, code, reason)`.
fn storage_error(code: u16, reason: &str) -> FakeResponse {
    FakeResponse::json_status(
        code,
        format!(
            r#"{{"error":{{"code":{code},"message":"{reason}","errors":[{{"message":"{reason}","domain":"global","reason":"{reason}"}}]}}}}"#
        ),
    )
}

/// The parts of a `multipart/related` body: (headers text, content).
fn multipart_parts(body: &[u8], boundary: &str) -> Vec<(String, Bytes)> {
    let delim = format!("--{boundary}");
    let text = body.to_vec();
    let mut out = Vec::new();
    let mut rest: &[u8] = &text;
    let find = |hay: &[u8], needle: &[u8]| hay.windows(needle.len()).position(|w| w == needle);
    while let Some(start) = find(rest, delim.as_bytes()) {
        rest = &rest[start + delim.len()..];
        if rest.starts_with(b"--") {
            break;
        }
        let Some(head_end) = find(rest, b"\r\n\r\n") else {
            break;
        };
        let headers = String::from_utf8_lossy(&rest[..head_end]).into_owned();
        let content_start = head_end + 4;
        let Some(next) = find(&rest[content_start..], delim.as_bytes()) else {
            break;
        };
        let content = &rest[content_start..content_start + next];
        let content = content.strip_suffix(b"\r\n").unwrap_or(content);
        out.push((headers, Bytes::copy_from_slice(content)));
        rest = &rest[content_start + next..];
    }
    out
}

impl FakeStorage {
    /// Go `newFakeStorage(t)`.
    fn new() -> FakeStorage {
        let state = Arc::new(Mutex::new(StorageState::default()));
        let shared = Arc::clone(&state);
        let server = FakeServer::start(move |req: &RecordedRequest| {
            let Some(bucket) = req.path.strip_prefix("/upload/storage/v1/b/") else {
                return storage_error(404, "notFound");
            };
            if req.method != "POST" {
                return storage_error(404, "notFound");
            }
            let bucket = bucket.trim_end_matches("/o").to_owned();
            let query: BTreeMap<String, String> =
                url::form_urlencoded::parse(req.query.as_deref().unwrap_or("").as_bytes())
                    .into_owned()
                    .collect();
            let fail_with = {
                let mut s = shared.lock().unwrap();
                s.queries.push(query.clone());
                s.fail_with
            };
            if fail_with != 0 {
                return storage_error(fail_with, "forbidden");
            }
            let content_type = req.header("content-type");
            let Some(boundary) = content_type
                .strip_prefix("multipart/")
                .and_then(|rest| rest.split("boundary=").nth(1))
                .map(|b| b.trim_matches('"').to_owned())
            else {
                return storage_error(400, "badRequest");
            };
            let parts = multipart_parts(&req.body, &boundary);
            let [(_, metadata), (_, data)] = parts.as_slice() else {
                return storage_error(400, "badRequest");
            };
            let Ok(metadata) = serde_json::from_slice::<serde_json::Value>(metadata) else {
                return storage_error(400, "badRequest");
            };
            let object_content_type = metadata["contentType"].as_str().unwrap_or("").to_owned();
            let name = query.get("name").cloned().unwrap_or_default();
            let key = format!("{bucket}/{name}");
            {
                let mut s = shared.lock().unwrap();
                let exists = s.objects.contains_key(&key);
                if query.get("ifGenerationMatch").map(String::as_str) == Some("0") && exists {
                    return storage_error(412, "conditionNotMet");
                }
                s.objects.insert(
                    key,
                    StoredObject {
                        content_type: object_content_type.clone(),
                        data: data.clone(),
                    },
                );
            }
            FakeResponse::json(
                serde_json::json!({
                    "kind": "storage#object", "bucket": bucket, "name": name, "generation": "1",
                    "size": data.len().to_string(), "contentType": object_content_type,
                })
                .to_string(),
            )
        });
        FakeStorage { server, state }
    }

    /// Go `clientOptions()` (with `STORAGE_EMULATOR_HOST` empty).
    fn offloader(&self, bucket: &str) -> GcsOffloader {
        let options = ClientOptions {
            endpoint: Some(format!("{}/storage/v1/", self.server.url())),
            without_authentication: true,
            ..ClientOptions::default()
        };
        GcsOffloader::new_with_env(bucket, &options, &MapEnv(Default::default()))
            .unwrap_or_else(|e| panic!("newGCSOffloader() error: {e}"))
    }

    fn object(&self, bucket: &str, name: &str) -> Option<StoredObject> {
        self.state
            .lock()
            .unwrap()
            .objects
            .get(&format!("{bucket}/{name}"))
            .cloned()
    }

    fn queries(&self) -> Vec<BTreeMap<String, String>> {
        self.state.lock().unwrap().queries.clone()
    }
}

fn upload_request(path: &str, data: &'static [u8], content_type: &str) -> GcsUploadRequest {
    GcsUploadRequest {
        path: path.to_owned(),
        data: Bytes::from_static(data),
        content_type: content_type.to_owned(),
    }
}

#[tokio::test]
async fn test_gcs_offloader_uploads_to_cloud_storage() {
    let ctx = Context::background();
    let fs = FakeStorage::new();
    let o = fs.offloader("test-bucket");

    let data: &'static [u8] = &[0x89, b'P', b'N', b'G', 0, 1, 2];
    let uri = o
        .upload_content(
            &ctx,
            upload_request("logs/2026-03-04/t/s_e_c0_p0.png", data, "image/png"),
        )
        .await
        .unwrap_or_else(|e| panic!("UploadContent() error: {e}"));
    assert_eq!(uri, "gs://test-bucket/logs/2026-03-04/t/s_e_c0_p0.png");
    let got = fs
        .object("test-bucket", "logs/2026-03-04/t/s_e_c0_p0.png")
        .expect("the object was not stored");
    assert_eq!(
        got,
        StoredObject {
            content_type: "image/png".to_owned(),
            data: Bytes::from_static(data)
        }
    );
    // The write must be conditional, so that it never overwrites an object.
    let queries = fs.queries();
    assert_eq!(queries.len(), 1, "requests");
    assert_eq!(
        queries[0].get("ifGenerationMatch").map(String::as_str),
        Some("0"),
        "ifGenerationMatch (create only if the object does not exist)"
    );
    o.close().await.unwrap();
}

#[tokio::test]
async fn test_gcs_offloader_existing_object_counts_as_uploaded() {
    let ctx = Context::background();
    let fs = FakeStorage::new();
    let o = fs.offloader("test-bucket");

    o.upload_content(
        &ctx,
        upload_request("same/object.txt", b"first", "text/plain"),
    )
    .await
    .unwrap_or_else(|e| panic!("first UploadContent() error: {e}"));
    // A retried write of the same part finds the object of the first attempt.
    let uri = o
        .upload_content(
            &ctx,
            upload_request("same/object.txt", b"second", "text/plain"),
        )
        .await
        .unwrap_or_else(|e| {
            panic!("second UploadContent() error: {e}, want the 412 to count as success")
        });
    assert_eq!(uri, "gs://test-bucket/same/object.txt");
    assert_eq!(
        fs.object("test-bucket", "same/object.txt").unwrap().data,
        Bytes::from_static(b"first"),
        "the first write must be kept"
    );
}

#[tokio::test]
async fn test_gcs_offloader_upload_errors() {
    for status in [403_u16, 404, 400] {
        let ctx = Context::background();
        let fs = FakeStorage::new();
        fs.state.lock().unwrap().fail_with = status;
        let o = fs.offloader("test-bucket");

        let err = o
            .upload_content(
                &ctx,
                upload_request("p.txt", b"PAYLOAD-MARKER", "text/plain"),
            )
            .await
            .expect_err("UploadContent() = nil error; want an error");
        // Go's `uri == ""` on error: a Rust error carries no URI.
        let code = match err.find(&mut |e| matches!(e, Error::Api(_))) {
            Some(Error::Api(api)) => Some(api.code),
            _ => None,
        };
        assert_eq!(
            code,
            Some(status),
            "{status}: error = {err}, want a googleapi error"
        );
        assert!(
            !err.to_string().contains("PAYLOAD-MARKER"),
            "the error contains the payload: {err}"
        );
    }
}

#[tokio::test]
async fn test_gcs_offloader_canceled_context() {
    let fs = FakeStorage::new();
    let o = fs.offloader("test-bucket");

    let ctx = Context::background().with_cancel();
    ctx.cancel();
    let got = o
        .upload_content(&ctx, upload_request("p.txt", b"x", "text/plain"))
        .await;
    assert!(
        got.is_err(),
        "UploadContent() = {got:?} with a canceled context; want an error"
    );
    assert!(
        fs.object("test-bucket", "p.txt").is_none(),
        "the object was stored although the context was canceled"
    );
}

// parity: adapted: Rust has no nil error, so the "nil" case is left out
#[test]
fn test_is_precondition_failed() {
    let api = |code: u16| {
        Error::Api(Box::new(ApiError {
            code,
            ..ApiError::default()
        }))
    };
    let tests = [
        ("412", api(412), true),
        ("wrapped 412", Error::wrap("write", api(412)), true),
        ("403", api(403), false),
        ("404", api(404), false),
        ("other error", Error::msg("412"), false),
    ];
    for (name, err, want) in tests {
        assert_eq!(is_precondition_failed(&err), want, "{name}: {err}");
    }
}

#[test]
fn test_gcs_object_path() {
    let ts = utc(2026, 3, 4, 5, 6, 7, 0);
    let eastern = FixedOffset::west_opt(5 * 3600).unwrap();
    let late = eastern
        .with_ymd_and_hms(2026, 3, 4, 23, 30, 0)
        .unwrap()
        .with_timezone(&Utc);
    #[allow(clippy::type_complexity)]
    let tests: [(
        &str,
        &str,
        chrono::DateTime<Utc>,
        [&str; 3],
        usize,
        usize,
        &str,
        &str,
    ); 8] = [
        (
            "all parts",
            "",
            ts,
            ["trace", "span", "event"],
            1,
            2,
            ".png",
            "2026-03-04/trace/span_event_c1_p2.png",
        ),
        (
            "prefix",
            "agent-logs",
            ts,
            ["trace", "span", "event"],
            0,
            0,
            ".txt",
            "agent-logs/2026-03-04/trace/span_event_c0_p0.txt",
        ),
        (
            "prefix with slashes",
            "/a/b/",
            ts,
            ["trace", "span", "event"],
            0,
            0,
            ".txt",
            "a/b/2026-03-04/trace/span_event_c0_p0.txt",
        ),
        (
            "only slashes in the prefix",
            "//",
            ts,
            ["trace", "span", "event"],
            0,
            0,
            ".txt",
            "2026-03-04/trace/span_event_c0_p0.txt",
        ),
        (
            "the date is in UTC",
            "",
            late,
            ["trace", "span", "event"],
            0,
            0,
            ".txt",
            "2026-03-05/trace/span_event_c0_p0.txt",
        ),
        (
            "missing ids",
            "",
            ts,
            ["", "", ""],
            0,
            0,
            ".bin",
            "2026-03-04/no_trace/no_span_no_event_c0_p0.bin",
        ),
        (
            "multi-digit ordinals",
            "",
            ts,
            ["trace", "span", "event"],
            12,
            345,
            ".jpg",
            "2026-03-04/trace/span_event_c12_p345.jpg",
        ),
        // The three runes of "é?*" each become one underscore. Between "x_y" and "c0" are the
        // separator, those three and the underscore that introduces the content ordinal.
        (
            "unsafe characters are replaced",
            "",
            ts,
            ["a/b c", "x\ny", "é?*"],
            0,
            0,
            ".txt",
            "2026-03-04/a_b_c/x_y_____c0_p0.txt",
        ),
    ];
    for (name, prefix, ts, [trace, span, event], content_ord, part_idx, ext, want) in tests {
        assert_eq!(
            gcs_object_path(prefix, ts, trace, span, event, content_ord, part_idx, ext),
            want,
            "{name}"
        );
    }
}

/// A text part and a PNG part in one user message.
fn text_and_image(text: &str, mime_type: &str, data: &[u8]) -> Content {
    Content {
        role: "user".to_owned(),
        parts: vec![
            Part {
                text: text.to_owned(),
                ..Part::default()
            },
            Part {
                inline_data: Some(Blob {
                    mime_type: mime_type.to_owned(),
                    data: Bytes::copy_from_slice(data),
                    ..Blob::default()
                }),
                ..Part::default()
            },
        ],
    }
}

/// A Go `map[string]any` content part literal.
fn part_of(pairs: &[(&str, RowValue)]) -> Row {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), v.clone()))
        .collect()
}

#[tokio::test(start_paused = true)]
async fn test_resolve_uploads_rewrites_the_entries() {
    let text = format!("SECRET-TEXT-MARKER{}", "-".repeat(100));
    let mut row = queued_row(&opts(50), &[text_and_image(&text, "image/png", &[1, 2, 3])]);
    let off = Arc::new(HookOffloader::default());
    let bp = bare(Config {
        gcs_offloader: as_off(&off),
        gcs_path_prefix: "logs".to_owned(),
        connection_id: "us.conn".to_owned(),
        ..Config::zero()
    });

    resolve(
        &bp,
        &Context::background(),
        false,
        std::slice::from_mut(&mut row),
    )
    .await;

    let text_uri = "gs://fake-bucket/logs/2026-03-04/trace1/span1_event1_c0_p0.txt";
    let image_uri = "gs://fake-bucket/logs/2026-03-04/trace1/span1_event1_c0_p1.png";
    let want = vec![
        part_of(&[
            ("part_index", RowValue::Int(0)),
            ("storage_mode", RowValue::from("GCS_REFERENCE")),
            ("mime_type", RowValue::from("text/plain")),
            ("text", RowValue::from(format!("{text}... [OFFLOADED]"))),
            ("uri", RowValue::from(text_uri)),
            (
                "object_ref",
                RowValue::Map(part_of(&[
                    ("uri", RowValue::from(text_uri)),
                    ("authorizer", RowValue::from("us.conn")),
                    (
                        "details",
                        RowValue::from(
                            r#"{"gcs_metadata":{"content_type":"text/plain; charset=utf-8","size":118}}"#,
                        ),
                    ),
                ])),
            ),
        ]),
        part_of(&[
            ("part_index", RowValue::Int(1)),
            ("storage_mode", RowValue::from("GCS_REFERENCE")),
            ("mime_type", RowValue::from("image/png")),
            ("text", RowValue::from("[MEDIA OFFLOADED]")),
            ("uri", RowValue::from(image_uri)),
            (
                "object_ref",
                RowValue::Map(part_of(&[
                    ("uri", RowValue::from(image_uri)),
                    ("authorizer", RowValue::from("us.conn")),
                    (
                        "details",
                        RowValue::from(r#"{"gcs_metadata":{"content_type":"image/png","size":3}}"#),
                    ),
                ])),
            ),
        ]),
    ];
    assert_eq!(parts(&row), want, "content_parts");
    assert!(
        row.raw_parts.is_empty(),
        "the row still has its queued payloads"
    );
    // On a successful resolution with no deferred uploads left, resolveUploads cleans up the
    // deferred state before the caller settles the batch.
    assert!(
        row.deferred.is_none(),
        "the row still has deferred state after resolveUploads"
    );
    let got: Vec<(String, String, Bytes)> = off
        .uploads()
        .into_iter()
        .map(|c: UploadAttempt| (c.path, c.content_type, c.data))
        .collect();
    assert_eq!(
        got,
        vec![
            (
                "logs/2026-03-04/trace1/span1_event1_c0_p0.txt".to_owned(),
                "text/plain; charset=utf-8".to_owned(),
                Bytes::from(text.clone().into_bytes())
            ),
            (
                "logs/2026-03-04/trace1/span1_event1_c0_p1.png".to_owned(),
                "image/png".to_owned(),
                Bytes::from_static(&[1, 2, 3])
            ),
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn test_resolve_uploads_authorizer() {
    let uri = "gs://fake-bucket/2026-03-04/trace1/span1_event1_c0_p0.png";
    let details = r#"{"gcs_metadata":{"content_type":"image/png","size":1}}"#;
    let tests = [
        (
            "without a connection there is no authorizer",
            "",
            part_of(&[
                ("uri", RowValue::from(uri)),
                ("details", RowValue::from(details)),
            ]),
        ),
        (
            "the connection is the authorizer",
            "us.conn",
            part_of(&[
                ("uri", RowValue::from(uri)),
                ("details", RowValue::from(details)),
                ("authorizer", RowValue::from("us.conn")),
            ]),
        ),
    ];
    for (name, connection_id, want) in tests {
        let mut row = queued_row(&opts(50), &[image_content(&[7])]);
        let bp = bare(Config {
            gcs_offloader: as_off(&Arc::new(HookOffloader::default())),
            connection_id: connection_id.to_owned(),
            ..Config::zero()
        });

        resolve(
            &bp,
            &Context::background(),
            false,
            std::slice::from_mut(&mut row),
        )
        .await;

        // version is left out in both cases: adk-python writes NULL, and an empty string would
        // be an invalid object version.
        assert_eq!(parts(&row)[0].map("object_ref"), Some(&want), "{name}");
    }
}

/// One case of `TestResolveUploads_FailuresKeepTheInlineForm`.
struct FailureCase {
    name: &'static str,
    canceled: bool,
    timeout: Duration,
    hook: Option<OffloadHook>,
    uri: Option<UriFn>,
    want_log: &'static str,
    want_calls: usize,
}

// TestResolveUploads_FailuresKeepTheInlineForm checks that every way an upload can go wrong
// leaves text entries as they were queued and marks binary entries with [UPLOAD FAILED], and
// that none of them logs what was to be uploaded.
#[tokio::test(start_paused = true)]
async fn test_resolve_uploads_failures_keep_the_inline_form() {
    let cases = vec![
        FailureCase {
            name: "the offloader fails",
            canceled: false,
            timeout: Duration::ZERO,
            hook: Some(hook(|_, _| {
                Box::pin(async { Err(Error::msg("storage unavailable: SECRET-ERR-MARKER")) })
            })),
            uri: None,
            want_log: "error type *errors.errorString",
            want_calls: 2,
        },
        FailureCase {
            name: "the offloader panics",
            canceled: false,
            timeout: Duration::ZERO,
            hook: Some(hook(|_, _| Box::pin(async { panic!("PANIC-MARKER") }))),
            uri: None,
            want_log: "offloader panicked (string)",
            want_calls: 2,
        },
        FailureCase {
            name: "the offloader returns no URI",
            canceled: false,
            timeout: Duration::ZERO,
            hook: None,
            uri: Some(Arc::new(|_: &GcsUploadRequest| String::new())),
            want_log: "not a gs:// URI",
            want_calls: 2,
        },
        FailureCase {
            name: "the offloader returns a non-gs URI",
            canceled: false,
            timeout: Duration::ZERO,
            hook: None,
            uri: Some(Arc::new(|_: &GcsUploadRequest| {
                "https://example.com/SECRET-URL-MARKER".to_owned()
            })),
            want_log: "not a gs:// URI",
            want_calls: 2,
        },
        FailureCase {
            name: "the upload takes too long",
            canceled: false,
            timeout: Duration::from_millis(20),
            hook: Some(hook(|ctx, _| Box::pin(wait_done(ctx)))),
            uri: None,
            want_log: "deadline exceeded",
            want_calls: 2,
        },
        FailureCase {
            name: "the context is canceled",
            canceled: true,
            timeout: Duration::ZERO,
            hook: None,
            uri: None,
            want_log: "context canceled",
            want_calls: 0,
        },
    ];
    for tt in cases {
        let content = text_and_image(
            &format!("SECRET-TEXT-MARKER{}", "-".repeat(100)),
            r#"image/png; name="SECRET-MIME-PARAM""#,
            b"SECRET-IMAGE-MARKER",
        );
        let mut row = queued_row(&opts(50), std::slice::from_ref(&content));
        // Text keeps its inline cut; binary parts mark the failed upload.
        let inline = PartsOptions {
            offload: false,
            ..opts(50)
        };
        let mut want = parts(&queued_row(&inline, std::slice::from_ref(&content)));
        want[1].insert("text".to_owned(), RowValue::from(UPLOAD_FAILED_TEXT));
        let off = Arc::new(HookOffloader::new(tt.hook, tt.uri));
        let logger = Arc::new(RecordingLogger::default());
        let bp = bare(Config {
            gcs_offloader: as_off(&off),
            logger: as_log(&logger),
            ..Config::zero()
        });
        bp.set_upload_timeout(tt.timeout);
        let ctx = Context::background().with_cancel();
        if tt.canceled {
            ctx.cancel();
        }

        resolve(&bp, &ctx, false, std::slice::from_mut(&mut row)).await;
        // This test never requeues. Drop a deferred upload the way a successful write does;
        // that is what logs it. Other failures were already logged.
        bp.clear_deferred_uploads(std::slice::from_mut(&mut row));

        assert_eq!(parts(&row), want, "{}: content_parts changed", tt.name);
        assert!(
            row.raw_parts.is_empty(),
            "{}: queued payloads left",
            tt.name
        );
        assert_eq!(
            off.uploads().len(),
            tt.want_calls,
            "{}: uploads started",
            tt.name
        );
        assert!(
            logger.contains("GCS upload failed") && logger.contains(tt.want_log),
            "{}: the log lacks {:?} and \"GCS upload failed\":\n{logger}",
            tt.name,
            tt.want_log
        );
        for secret in [
            "SECRET-TEXT-MARKER",
            "SECRET-IMAGE-MARKER",
            "PANIC-MARKER",
            "SECRET-ERR-MARKER",
            "SECRET-URL-MARKER",
            "SECRET-MIME-PARAM",
        ] {
            assert!(
                !logger.contains(secret),
                "{}: the log contains {secret:?}:\n{logger}",
                tt.name
            );
        }
    }
}

#[tokio::test(start_paused = true)]
async fn test_resolve_uploads_one_failure_does_not_spoil_the_others() {
    let mut row = queued_row(
        &opts(50),
        &[
            image_content(&[1]),
            image_content(&[2]),
            image_content(&[3]),
        ],
    );
    let off = hooked(|_, req| {
        Box::pin(async move {
            if req.path.contains("_c1_") {
                return Err(Error::msg("boom"));
            }
            Ok(())
        })
    });
    let bp = bare(Config {
        gcs_offloader: as_off(&off),
        logger: as_log(&Arc::new(RecordingLogger::default())),
        ..Config::zero()
    });

    resolve(
        &bp,
        &Context::background(),
        false,
        std::slice::from_mut(&mut row),
    )
    .await;

    let got: Vec<String> = parts(&row)
        .iter()
        .map(|p| field(p, "storage_mode").to_owned())
        .collect();
    assert_eq!(got, ["GCS_REFERENCE", "INLINE", "GCS_REFERENCE"]);
}

#[tokio::test(start_paused = true)]
async fn test_resolve_uploads_uploads_have_a_deadline() {
    let mut row = queued_row(&opts(50), &[image_content(&[1])]);
    let off = Arc::new(HookOffloader::default());
    let bp = bare(Config {
        gcs_offloader: as_off(&off),
        ..Config::zero()
    });

    resolve(
        &bp,
        &Context::background(),
        false,
        std::slice::from_mut(&mut row),
    )
    .await;

    let calls = off.uploads();
    assert_eq!(calls.len(), 1, "uploads");
    // An upload may take 10 seconds, no more and, as it starts at once, not much less.
    let want = Duration::from_secs(10);
    let got = &calls[0];
    assert!(
        got.has_deadline && got.remaining > want - Duration::from_secs(5) && got.remaining <= want,
        "the upload context has deadline {} in {:?}; want one in (5s, 10s]",
        got.has_deadline,
        got.remaining
    );
}

/// Go `barrier`: releases the callers of `wait` once `n` have arrived, or fails them after 5s.
pub(super) struct Barrier {
    n: usize,
    arrived: std::sync::atomic::AtomicUsize,
    open: tokio::sync::Notify,
    opened: AtomicBool,
}

impl Barrier {
    pub(super) fn new(n: usize) -> Arc<Barrier> {
        Arc::new(Barrier {
            n,
            arrived: std::sync::atomic::AtomicUsize::new(0),
            open: tokio::sync::Notify::new(),
            opened: AtomicBool::new(false),
        })
    }

    pub(super) async fn wait(&self) -> Result<(), Error> {
        let notified = self.open.notified();
        if self.arrived.fetch_add(1, Ordering::SeqCst) + 1 == self.n {
            self.opened.store(true, Ordering::SeqCst);
            self.open.notify_waiters();
        }
        if self.opened.load(Ordering::SeqCst) {
            return Ok(());
        }
        match tokio::time::timeout(Duration::from_secs(5), notified).await {
            Ok(()) => Ok(()),
            Err(_) => Err(Error::msg(format!("fewer than {} callers arrived", self.n))),
        }
    }
}

#[tokio::test(start_paused = true)]
async fn test_resolve_uploads_concurrency_is_bounded() {
    for (name, limit, want) in [
        ("the configured limit", 3, 3),
        ("a limit of zero means the default", 0, 8),
        ("a negative limit means the default", -1, 8),
    ] {
        let contents: Vec<Content> = (0..25_u8).map(|i| image_content(&[i])).collect();
        let mut row = queued_row(&opts(50), &contents);
        // The first `want` uploads wait for each other, which shows that that many run at
        // once; the pause that follows gives more a chance to start if the limit does not hold.
        let gate = Barrier::new(want);
        let off = hooked(move |_, _| {
            let gate = Arc::clone(&gate);
            Box::pin(async move {
                gate.wait().await?;
                tokio::time::sleep(Duration::from_millis(10)).await;
                Ok(())
            })
        });
        let bp = bare(Config {
            gcs_offloader: as_off(&off),
            gcs_max_concurrent_uploads: limit,
            logger: as_log(&Arc::new(RecordingLogger::default())),
            ..Config::zero()
        });

        resolve(
            &bp,
            &Context::background(),
            false,
            std::slice::from_mut(&mut row),
        )
        .await;

        assert_eq!(
            off.max_concurrent(),
            want,
            "{name}: uploads at the same time"
        );
        assert_eq!(off.uploads().len(), contents.len(), "{name}: uploads made");
        for (i, part) in parts(&row).iter().enumerate() {
            assert_eq!(
                field(part, "storage_mode"),
                "GCS_REFERENCE",
                "{name}: part {i}"
            );
        }
    }
}

#[tokio::test(start_paused = true)]
async fn test_resolve_uploads_without_an_offloader_the_parts_stay_inline() {
    let mut row = queued_row(&opts(50), &[image_content(&[1])]);
    let inline = PartsOptions {
        max_len: 1000,
        ..PartsOptions::default()
    };
    let want = parts(&queued_row(&inline, &[image_content(&[1])]));
    let logger = Arc::new(RecordingLogger::default());
    let bp = bare(Config {
        logger: as_log(&logger),
        ..Config::zero()
    });

    resolve(
        &bp,
        &Context::background(),
        false,
        std::slice::from_mut(&mut row),
    )
    .await;

    assert_eq!(parts(&row), want, "content_parts changed");
    assert!(row.raw_parts.is_empty(), "queued payloads left");
    assert!(row.deferred.is_none(), "deferred state left");
    // No upload is attempted, so none fails.
    assert!(logger.lines().is_empty(), "the log is not empty:\n{logger}");
}

// parity: adapted: a slot is a usize in Rust, so Go's slot -1 is usize::MAX
#[tokio::test(start_paused = true)]
async fn test_resolve_uploads_rows_without_uploads_are_left_alone() {
    let one_part = || RowValue::Parts(vec![part_of(&[("part_index", RowValue::Int(0))])]);
    let image = |slot: usize| PendingUpload {
        slot,
        mime_type: "image/png".to_owned(),
        data: Bytes::from_static(&[1]),
        ..PendingUpload::default()
    };
    let plain = QueuedRow::new(part_of(&[
        ("event_type", RowValue::from("X")),
        ("content_parts", one_part()),
    ]));
    // Pending uploads that name an entry which is not there are skipped.
    let mut out_of_range = QueuedRow::new(part_of(&[("content_parts", one_part())]));
    out_of_range.raw_parts = vec![image(1), image(usize::MAX)];
    let mut no_parts = QueuedRow::new(Row::new());
    no_parts.raw_parts = vec![image(0)];
    let off = Arc::new(HookOffloader::default());
    let bp = bare(Config {
        gcs_offloader: as_off(&off),
        ..Config::zero()
    });

    let mut batch = vec![plain, out_of_range, no_parts];
    resolve(&bp, &Context::background(), false, &mut batch).await;

    assert!(off.uploads().is_empty(), "uploads were made");
    assert_eq!(
        batch[0].row.get("content_parts"),
        Some(&one_part()),
        "content_parts changed"
    );
    for (i, q) in batch[1..].iter().enumerate() {
        assert!(
            q.raw_parts.is_empty(),
            "row {i} still has its queued payloads"
        );
    }
}

#[test]
fn test_batch_processor_object_path() {
    let bp = BatchProcessor::bare(
        Config {
            gcs_path_prefix: "p".to_owned(),
            ..Config::zero()
        },
        0,
    );
    let u = PendingUpload {
        content_ord: 2,
        part_idx: 3,
        mime_type: "image/jpeg".to_owned(),
        ..PendingUpload::default()
    };

    // from the row
    let ts = FixedOffset::west_opt(5 * 3600)
        .unwrap()
        .with_ymd_and_hms(2026, 3, 4, 23, 30, 0)
        .unwrap()
        .with_timezone(&Utc);
    let row = part_of(&[
        ("timestamp", RowValue::Timestamp(ts)),
        ("event_id", RowValue::from("e")),
        ("trace_id", RowValue::from("t")),
        ("span_id", RowValue::from("s")),
    ]);
    assert_eq!(bp.object_path(&row, &u), "p/2026-03-05/t/s_e_c2_p3.jpg");

    // a row without ids or a time
    let before = Utc::now().format("%Y-%m-%d").to_string();
    let got = bp.object_path(&Row::new(), &u);
    let after = Utc::now().format("%Y-%m-%d").to_string();
    let rest = "/no_trace/no_span_no_event_c2_p3.jpg";
    assert!(
        got == format!("p/{before}{rest}") || got == format!("p/{after}{rest}"),
        "objectPath() = {got:?}, want today's date between p/ and {rest}"
    );
}
