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

//! Test support of the `batch_processor_test.go` ports: Go's `arrow_test_helper_test.go`
//! helpers (`decodeArrowRows`, `arrowWriterSchemaColumns`, `collectRows`, `expectNoRequest`,
//! `eventTypes`, `rowsOfType`, `onlyRowOfType`), the shared fakes of `batch_processor_test.go`
//! (`committedStreamTestServer`, `scriptedCommittedStreamServer`, `exactlyOnceTestConfig`,
//! `newErrorClassificationProcessor`, `hookRecordingLogger`, the stalling / delayed /
//! arrival-recording / hook write servers), Go's `fakeBigQueryWriteServer` (from
//! `bigquery_agent_analytics_plugin_test.go`; the `FakeWriteServer` default), and the
//! `gcs_offloader_test.go` helpers these tests use (`fakeOffloader`, `imageContent`,
//! `imageContents`, `queuedRow`). Go's per-test gRPC servers are hooks on
//! [`FakeWriteServer`]; Go's `requests` channels are unbounded mpsc channels.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use chrono::{DateTime, TimeZone, Utc};
use gadk_genai::{Blob, Content, Part};
use gadk_platform::BoxFuture;
use tokio::sync::mpsc;
use tokio::time::Instant;

use crate::batch_processor::BatchProcessor;
use crate::bigquery::fake::RecordingLogger;
use crate::config::{Config, GcsContentOffloader, GcsUploadRequest, Logger, RetryConfig};
use crate::context::{Context, Metadata};
use crate::error::{Code, Error};
use crate::formatter::{PartsCollector, PartsOptions};
use crate::row::{QueuedRow, Row, RowValue, from_json_value};
use crate::storagepb::{
    AppendRowsRequest, AppendRowsResponse, CreateWriteStreamRequest, FinalizeWriteStreamRequest,
    FinalizeWriteStreamResponse, WriteStream, append_rows_request,
};
use crate::testutil::fake_write_server::{AppendRowsServerStream, FakeWriteServer, UnaryCall};
use crate::write::stream_writer::tests::{MockStream, scripted};

/// The table `scripted_committed_stream_server` names its streams after.
pub(crate) const TEST_PARENT: &str = "projects/test-proj/datasets/test-ds/tables/agent_events";

/// The `_default` stream of [`TEST_PARENT`].
pub(crate) const TEST_DEFAULT_STREAM: &str =
    "projects/test-proj/datasets/test-ds/tables/agent_events/_default";

/// `<TEST_PARENT>/streams/committed-<n>` (Go's scripted server's names).
pub(crate) fn committed(n: usize) -> String {
    format!("{TEST_PARENT}/streams/committed-{n}")
}

// ---------------------------------------------------------------------------------------------
// Rows
// ---------------------------------------------------------------------------------------------

/// A Go row literal `map[string]any{k: v, ...}` as a [`QueuedRow`].
pub(crate) fn qrow(pairs: &[(&str, RowValue)]) -> QueuedRow {
    QueuedRow::new(
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect(),
    )
}

/// Go `map[string]any{"event_type": eventType}`.
pub(crate) fn event_row(event_type: &str) -> QueuedRow {
    qrow(&[("event_type", RowValue::from(event_type))])
}

/// Go `time.Date(y, m, d, h, min, s, ns, time.UTC)`.
pub(crate) fn utc(y: i32, m: u32, d: u32, h: u32, min: u32, s: u32, ns: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(y, m, d, h, min, s)
        .single()
        .expect("valid date")
        + chrono::TimeDelta::nanoseconds(i64::from(ns))
}

// ---------------------------------------------------------------------------------------------
// Arrow decoding (arrow_test_helper_test.go)
// ---------------------------------------------------------------------------------------------

/// Go `decodeArrowRows(t, req)` ([`crate::testutil::arrow_rows::decode_arrow_rows`], shared
/// with the integration tests); panics when the request does not decode.
pub(crate) fn decode_arrow_rows(req: &AppendRowsRequest) -> Vec<Row> {
    crate::testutil::arrow_rows::decode_arrow_rows(req).unwrap_or_else(|e| panic!("{e}"))
}

/// Go `arrowWriterSchemaColumns(t, req)`.
#[allow(dead_code)] // consumer: 7b-3 (the plugin-level gcs_offloader_test.go ports)
pub(crate) fn arrow_writer_schema_columns(req: &AppendRowsRequest) -> Vec<String> {
    crate::testutil::arrow_rows::arrow_writer_schema_columns(req).unwrap_or_else(|e| panic!("{e}"))
}

/// Go `eventTypes(rows)`.
pub(crate) fn event_types(rows: &[Row]) -> Vec<String> {
    rows.iter()
        .map(|r| match r.get("event_type") {
            Some(RowValue::Str(s)) => s.clone(),
            _ => String::new(),
        })
        .collect()
}

/// Go `rowsOfType(rows, eventType)`.
pub(crate) fn rows_of_type(rows: &[Row], event_type: &str) -> Vec<Row> {
    rows.iter()
        .filter(|r| r.get("event_type") == Some(&RowValue::from(event_type)))
        .cloned()
        .collect()
}

/// Go `onlyRowOfType(t, rows, eventType)`.
#[allow(dead_code)] // consumer: 7b-3 (the plugin-level gcs_offloader_test.go ports)
pub(crate) fn only_row_of_type(rows: &[Row], event_type: &str) -> Row {
    let matches = rows_of_type(rows, event_type);
    assert_eq!(
        matches.len(),
        1,
        "got {} {event_type} rows, want 1; event types: {:?}",
        matches.len(),
        event_types(rows)
    );
    matches.into_iter().next().unwrap()
}

/// The `content_parts` of a decoded row (empty when left out).
pub(crate) fn content_parts_of(row: &Row) -> Vec<Row> {
    match row.get("content_parts") {
        Some(RowValue::Parts(parts)) => parts.clone(),
        _ => Vec::new(),
    }
}

/// A Go `chan *storagepb.AppendRowsRequest` (unbounded; Go's buffered channels never fill in
/// these tests).
pub(crate) type Requests = mpsc::UnboundedSender<AppendRowsRequest>;

/// A request channel.
pub(crate) fn requests() -> (Requests, mpsc::UnboundedReceiver<AppendRowsRequest>) {
    mpsc::unbounded_channel()
}

/// Go `collectRows(t, ch, done)`: decodes the requests arriving on `rx` until `done` holds (5s
/// timeout).
pub(crate) async fn collect_rows(
    rx: &mut mpsc::UnboundedReceiver<AppendRowsRequest>,
    done: impl Fn(&[Row]) -> bool,
) -> Vec<Row> {
    let mut rows = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !done(&rows) {
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Some(req)) => rows.extend(decode_arrow_rows(&req)),
            Ok(None) => panic!(
                "request channel closed; got event types {:?}",
                event_types(&rows)
            ),
            Err(_) => panic!(
                "timed out waiting for rows; got event types {:?}",
                event_types(&rows)
            ),
        }
    }
    rows
}

/// Go `untilEventType(eventType)`.
#[allow(dead_code)] // consumer: 7b-3 (the plugin-level gcs_offloader_test.go ports)
pub(crate) fn until_event_type(event_type: &str) -> impl Fn(&[Row]) -> bool + '_ {
    move |rows| {
        rows.iter()
            .any(|r| r.get("event_type") == Some(&RowValue::from(event_type)))
    }
}

/// Go `expectNoRequest(t, ch, d)`.
#[allow(dead_code)] // consumer: 7b-3 (the plugin-level gcs_offloader_test.go ports)
pub(crate) async fn expect_no_request(
    rx: &mut mpsc::UnboundedReceiver<AppendRowsRequest>,
    d: Duration,
) {
    if let Ok(Some(req)) = tokio::time::timeout(d, rx.recv()).await {
        panic!(
            "unexpected AppendRowsRequest with event types {:?}",
            event_types(&decode_arrow_rows(&req))
        );
    }
}

// ---------------------------------------------------------------------------------------------
// Processors and configs
// ---------------------------------------------------------------------------------------------

/// Go `bp.streamWriter.stream = m`.
pub(crate) async fn inject_stream(bp: &BatchProcessor, stream: MockStream) {
    bp.stream_writer.state.lock().await.stream = Some(Box::new(stream));
}

/// Go `DefaultConfig()` with `RetryConfig.MaxRetries = 0` (the usual nil-client test config).
pub(crate) fn no_retry_config() -> Config {
    let mut config = Config::default();
    config.retry_config.max_retries = 0;
    config
}

/// `logger` as a config logger.
pub(crate) fn as_logger(logger: &Arc<RecordingLogger>) -> Option<Arc<dyn Logger>> {
    Some(Arc::clone(logger) as Arc<dyn Logger>)
}

/// Go `exactlyOnceTestConfig()`.
pub(crate) fn exactly_once_test_config() -> Config {
    Config {
        project_id: "test-proj".to_owned(),
        dataset_id: "test-ds".to_owned(),
        table_name: "agent_events".to_owned(),
        exactly_once_delivery: true,
        retry_config: RetryConfig {
            max_retries: 2,
            initial_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(5),
            multiplier: 1.0,
        },
        ..Config::default()
    }
}

/// Go `NewBatchProcessor(ctx, writeClient, streamName, config)` that must succeed.
pub(crate) async fn processor(
    ctx: &Context,
    client: Option<crate::write::gapic::WriteClient>,
    stream_name: &str,
    config: Config,
) -> Arc<BatchProcessor> {
    BatchProcessor::new(ctx, client, stream_name, config)
        .await
        .unwrap_or_else(|e| panic!("NewBatchProcessor error: {e}"))
}

/// Go `newErrorClassificationProcessorForClient(t, writeClient, exactlyOnce)`: three retries
/// with millisecond backoff; a committed stream with `exactly_once`, else the `_default`
/// stream. Close it at the end of the test (Go's `t.Cleanup(bp.Close)`).
pub(crate) async fn new_error_classification_processor(
    server: &FakeWriteServer,
    exactly_once: bool,
) -> Arc<BatchProcessor> {
    let client = server.client().expect("client");
    new_error_classification_processor_for_client(client, exactly_once).await
}

/// Go `newErrorClassificationProcessorForClient`.
pub(crate) async fn new_error_classification_processor_for_client(
    client: crate::write::gapic::WriteClient,
    exactly_once: bool,
) -> Arc<BatchProcessor> {
    let mut config = exactly_once_test_config();
    config.exactly_once_delivery = exactly_once;
    config.retry_config = RetryConfig {
        max_retries: 3,
        initial_delay: Duration::from_millis(1),
        max_delay: Duration::from_millis(2),
        multiplier: 1.0,
    };
    let stream_name = if exactly_once {
        ""
    } else {
        TEST_DEFAULT_STREAM
    };
    processor(&Context::background(), Some(client), stream_name, config).await
}

/// The `DropStats` with only `append_failure_drops` (and the total) set.
pub(crate) fn append_failure_stats(n: i64) -> crate::config::DropStats {
    crate::config::DropStats {
        append_failure_drops: n,
        total_drops: n,
        ..crate::config::DropStats::default()
    }
}

// ---------------------------------------------------------------------------------------------
// Write servers
// ---------------------------------------------------------------------------------------------

/// Go `status.Error(code, msg)` on the server side.
pub(crate) fn status(code: Code, message: &str) -> tonic::Status {
    tonic::Status::new(tonic::Code::from_i32(code.as_u32() as i32), message)
}

/// Go `fakeBigQueryWriteServer{requests: ch}`: the default AppendRows behaviour (an empty
/// response per request) that also sends every request to `requests`.
pub(crate) fn recording_server(server: &FakeWriteServer, requests: Requests) {
    server.on_append_rows(Arc::new(move |mut stream: AppendRowsServerStream| {
        let requests = requests.clone();
        Box::pin(async move {
            while let Some(req) = stream.recv().await? {
                let _ = requests.send(req);
                stream.send(AppendRowsResponse::default()).await?;
            }
            Ok(())
        })
    }));
}

/// Go `stallingBigQueryWriteServer`: reads one request, then waits for the client to go.
pub(crate) fn stalling_server(server: &FakeWriteServer) {
    server.on_append_rows(Arc::new(|mut stream: AppendRowsServerStream| {
        Box::pin(async move {
            stream.recv().await?;
            stream.cancelled().await;
            Err(tonic::Status::cancelled("context canceled"))
        })
    }));
}

/// Go `stallingWriteServer{received}`: reads the first request of each stream, signals
/// `received` (or gives up when the client goes), then waits for the client to go.
pub(crate) fn stalling_write_server(server: &FakeWriteServer, received: mpsc::Sender<()>) {
    server.on_append_rows(Arc::new(move |mut stream: AppendRowsServerStream| {
        let received = received.clone();
        Box::pin(async move {
            stream.recv().await?;
            tokio::select! {
                _ = received.send(()) => {}
                () = stream.cancelled() => return Err(tonic::Status::cancelled("context canceled")),
            }
            stream.cancelled().await;
            Err(tonic::Status::cancelled("context canceled"))
        })
    }));
}

/// Go `delayedBigQueryWriteServer`'s settings.
#[derive(Clone, Default)]
pub(crate) struct Delayed {
    /// Go `delay`.
    pub(crate) delay: Duration,
    /// Go `stallAfterReq` (0 = never).
    pub(crate) stall_after_req: usize,
    /// Go `requests`.
    pub(crate) requests: Option<Requests>,
    /// Go `received`: signalled (without blocking) as each request arrives.
    pub(crate) received: Option<mpsc::Sender<()>>,
}

/// Go `delayedBigQueryWriteServer`; returns its request counter (Go `reqCount`).
pub(crate) fn delayed_server(server: &FakeWriteServer, d: Delayed) -> Arc<AtomicUsize> {
    let req_count = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&req_count);
    server.on_append_rows(Arc::new(move |mut stream: AppendRowsServerStream| {
        let d = d.clone();
        let count = Arc::clone(&count);
        Box::pin(async move {
            while let Some(req) = stream.recv().await? {
                if let Some(received) = &d.received {
                    let _ = received.try_send(());
                }
                let n = count.fetch_add(1, Ordering::SeqCst) + 1;
                if d.stall_after_req > 0 && n > d.stall_after_req {
                    stream.cancelled().await;
                    return Err(tonic::Status::cancelled("context canceled"));
                }
                tokio::select! {
                    () = tokio::time::sleep(d.delay) => {}
                    () = stream.cancelled() => return Err(tonic::Status::cancelled("context canceled")),
                }
                if let Some(requests) = &d.requests {
                    let _ = requests.send(req);
                }
                stream.send(AppendRowsResponse::default()).await?;
            }
            Ok(())
        })
    }));
    req_count
}

/// Go `arrivalRecordingBigQueryWriteServer{delay, requests}`: records each request on arrival,
/// then answers after `delay` (unless the client goes first).
pub(crate) fn arrival_recording_server(
    server: &FakeWriteServer,
    delay: Duration,
    requests: Requests,
) {
    server.on_append_rows(Arc::new(move |mut stream: AppendRowsServerStream| {
        let requests = requests.clone();
        Box::pin(async move {
            while let Some(req) = stream.recv().await? {
                let _ = requests.send(req);
                tokio::select! {
                    () = tokio::time::sleep(delay) => {}
                    () = stream.cancelled() => return Err(tonic::Status::cancelled("context canceled")),
                }
                stream.send(AppendRowsResponse::default()).await?;
            }
            Ok(())
        })
    }));
}

/// Go `dynamicDelayBigQueryWriteServer{delay, onAppend, requests}`: each request waits for
/// `delay` (ns, read when the request arrives), and a request on a stream the client has
/// already given up on is not recorded.
pub(crate) fn dynamic_delay_server(
    server: &FakeWriteServer,
    delay: Arc<AtomicI64>,
    on_append: Option<Arc<dyn Fn() + Send + Sync>>,
    requests: Option<Requests>,
) {
    server.on_append_rows(Arc::new(move |mut stream: AppendRowsServerStream| {
        let delay = Arc::clone(&delay);
        let on_append = on_append.clone();
        let requests = requests.clone();
        Box::pin(async move {
            while let Some(req) = stream.recv().await? {
                if let Some(on_append) = &on_append {
                    on_append();
                }
                let d = delay.load(Ordering::SeqCst);
                if d > 0 {
                    tokio::select! {
                        () = tokio::time::sleep(Duration::from_nanos(d.unsigned_abs())) => {}
                        () = stream.cancelled() => {
                            return Err(tonic::Status::cancelled("context canceled"));
                        }
                    }
                }
                // Go `stream.Context().Err()`: a client that has gone ends the stream first.
                if tokio::time::timeout(Duration::ZERO, stream.cancelled())
                    .await
                    .is_ok()
                {
                    return Err(tonic::Status::cancelled("context canceled"));
                }
                if let Some(requests) = &requests {
                    let _ = requests.send(req);
                }
                stream.send(AppendRowsResponse::default()).await?;
            }
            Ok(())
        })
    }));
}

/// Go `req.GetArrowRows().GetRows().GetRowCount()`.
pub(crate) fn row_count(req: &AppendRowsRequest) -> i64 {
    match &req.rows {
        #[allow(deprecated)]
        Some(append_rows_request::Rows::ArrowRows(data)) => {
            data.rows.as_ref().map_or(0, |r| r.row_count)
        }
        _ => 0,
    }
}

/// Go `partsOptions{maxLen: 1000, offload: true, inlineLimit: 10[, reserveBytes:
/// bp.tryReserveUploadBytes]}`.
pub(crate) fn offload_opts(bp: Option<&Arc<BatchProcessor>>) -> PartsOptions {
    PartsOptions {
        max_len: 1000,
        offload: true,
        inline_limit: 10,
        reserve_bytes: bp.and_then(reserve_with),
        ..PartsOptions::default()
    }
}

/// Go `hookBigQueryWriteServer{onAppend, requests}`: `on_append` runs (and is awaited) before
/// each request is recorded and answered.
pub(crate) fn hook_server(
    server: &FakeWriteServer,
    on_append: Option<Arc<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync>>,
    requests: Option<Requests>,
) {
    server.on_append_rows(Arc::new(move |mut stream: AppendRowsServerStream| {
        let on_append = on_append.clone();
        let requests = requests.clone();
        Box::pin(async move {
            while let Some(req) = stream.recv().await? {
                if let Some(on_append) = &on_append {
                    on_append().await;
                }
                if let Some(requests) = &requests {
                    let _ = requests.send(req);
                }
                stream.send(AppendRowsResponse::default()).await?;
            }
            Ok(())
        })
    }));
}

/// Go `committedStreamTestServer`'s recorded state and settings.
#[derive(Default)]
pub(crate) struct CommittedState {
    /// Go `createdStreamName`.
    pub(crate) created_stream_name: String,
    /// Go `createErr` (a server status).
    pub(crate) create_err: Option<(Code, String)>,
    /// Go `appendStreamNames`.
    pub(crate) append_stream_names: Vec<String>,
    /// Go `appendOffsets` (`-1` without one).
    pub(crate) append_offsets: Vec<i64>,
    /// Go `appendHasOffsets`.
    pub(crate) append_has_offsets: Vec<bool>,
    /// Go `failAppendAttemptN` (0 = never).
    pub(crate) fail_append_attempt_n: usize,
    /// Go `appendAttempts`.
    pub(crate) append_attempts: usize,
}

/// Go `committedStreamTestServer` (its `createReqs` / `finalizeReqs` are the fake's
/// `create_calls()` / `finalize_calls()`).
pub(crate) fn committed_stream_server(
    server: &FakeWriteServer,
    state: CommittedState,
) -> Arc<Mutex<CommittedState>> {
    let state = Arc::new(Mutex::new(state));
    let create_state = Arc::clone(&state);
    server.on_create_write_stream(Arc::new(
        move |call: UnaryCall<CreateWriteStreamRequest>| {
            let s = create_state.lock().unwrap();
            let result = match &s.create_err {
                Some((code, msg)) => Err(status(*code, msg)),
                None => Ok(WriteStream {
                    name: s.created_stream_name.clone(),
                    r#type: call.request.write_stream.as_ref().map_or(0, |w| w.r#type),
                    ..WriteStream::default()
                }),
            };
            Box::pin(async move { result })
        },
    ));
    server.on_finalize_write_stream(Arc::new(|_| {
        Box::pin(async { Ok(FinalizeWriteStreamResponse::default()) })
    }));
    let append_state = Arc::clone(&state);
    server.on_append_rows(Arc::new(move |mut stream: AppendRowsServerStream| {
        let state = Arc::clone(&append_state);
        Box::pin(async move {
            while let Some(req) = stream.recv().await? {
                let should_fail = {
                    let mut s = state.lock().unwrap();
                    s.append_attempts += 1;
                    let attempt = s.append_attempts;
                    s.append_stream_names.push(req.write_stream.clone());
                    match req.offset {
                        Some(o) => {
                            s.append_has_offsets.push(true);
                            s.append_offsets.push(o.value);
                        }
                        None => {
                            s.append_has_offsets.push(false);
                            s.append_offsets.push(-1);
                        }
                    }
                    s.fail_append_attempt_n > 0 && attempt == s.fail_append_attempt_n
                };
                if should_fail {
                    return Err(status(Code::Unavailable, "transient stream error"));
                }
                stream.send(AppendRowsResponse::default()).await?;
            }
            Ok(())
        })
    }));
    state
}

/// Go `scriptedCommittedStreamServer.onFinalize`.
pub(crate) type OnFinalize = Arc<
    dyn Fn(
            UnaryCall<FinalizeWriteStreamRequest>,
        ) -> BoxFuture<'static, Result<FinalizeWriteStreamResponse, tonic::Status>>
        + Send
        + Sync,
>;

/// Go `scriptedCommittedStreamServer`: `CreateWriteStream` names streams from `stream_names`
/// (then `committed-<n>`), `FinalizeWriteStream` records and runs `on_finalize`, AppendRows is
/// T8's [`scripted`] (`on_append(attempt, req)`).
pub(crate) struct ScriptedServer {
    /// The fake (Go's `createReqs` = `create_calls()`, `finalizeReqs` / `finalizeMetadata` =
    /// `finalize_calls()`).
    pub(crate) server: FakeWriteServer,
    /// Go `appendStreamNames` / `appendOffsets` (offset `-1` without one).
    pub(crate) delivered: crate::write::stream_writer::tests::Delivered,
}

impl ScriptedServer {
    /// Go `&scriptedCommittedStreamServer{streamNames, onAppend, onFinalize}`.
    pub(crate) fn new(
        stream_names: Vec<String>,
        on_append: impl Fn(usize, &AppendRowsRequest) -> Result<AppendRowsResponse, tonic::Status>
        + Send
        + Sync
        + 'static,
        on_finalize: Option<OnFinalize>,
    ) -> ScriptedServer {
        let server = FakeWriteServer::new();
        let calls = Arc::new(AtomicUsize::new(0));
        server.on_create_write_stream(Arc::new(
            move |call: UnaryCall<CreateWriteStreamRequest>| {
                let idx = calls.fetch_add(1, Ordering::SeqCst);
                let name = stream_names
                    .get(idx)
                    .cloned()
                    .unwrap_or_else(|| committed(idx + 1));
                let r#type = call.request.write_stream.as_ref().map_or(0, |w| w.r#type);
                Box::pin(async move {
                    Ok(WriteStream {
                        name,
                        r#type,
                        ..WriteStream::default()
                    })
                })
            },
        ));
        if let Some(on_finalize) = on_finalize {
            server.on_finalize_write_stream(on_finalize);
        }
        let delivered = scripted(&server, on_append);
        ScriptedServer { server, delivered }
    }

    /// Go `createCount()`.
    pub(crate) fn create_count(&self) -> usize {
        self.server.create_calls().len()
    }

    /// Go `attempts()`.
    pub(crate) fn attempts(&self) -> usize {
        self.delivered.lock().unwrap().len()
    }

    /// Go `appendStreamNames`.
    pub(crate) fn append_stream_names(&self) -> Vec<String> {
        self.delivered
            .lock()
            .unwrap()
            .iter()
            .map(|(n, _)| n.clone())
            .collect()
    }

    /// Go `appendOffsets`.
    pub(crate) fn append_offsets(&self) -> Vec<i64> {
        self.delivered
            .lock()
            .unwrap()
            .iter()
            .map(|(_, o)| *o)
            .collect()
    }

    /// Go `finalizeReqs` names.
    pub(crate) fn finalized(&self) -> Vec<String> {
        self.server
            .finalize_calls()
            .into_iter()
            .map(|c| c.request.name)
            .collect()
    }

    /// Go `finalizeMetadata`.
    pub(crate) fn finalize_metadata(&self) -> Vec<Metadata> {
        self.server
            .finalize_calls()
            .into_iter()
            .map(|c| c.metadata)
            .collect()
    }
}

// ---------------------------------------------------------------------------------------------
// Loggers
// ---------------------------------------------------------------------------------------------

/// Go `hookRecordingLogger`: `before_record(msg)` runs before each `Printf` line is recorded.
pub(crate) struct HookRecordingLogger {
    /// Go's embedded `*recordingLogger`.
    pub(crate) inner: Arc<RecordingLogger>,
    /// Go `beforeRecord`.
    pub(crate) before_record: Box<dyn Fn(&str) + Send + Sync>,
}

impl Logger for HookRecordingLogger {
    fn printf(&self, args: std::fmt::Arguments<'_>) {
        let msg = args.to_string();
        (self.before_record)(&msg);
        self.inner.printf(format_args!("{msg}"));
    }

    fn println(&self, args: std::fmt::Arguments<'_>) {
        self.inner.println(args);
    }
}

// ---------------------------------------------------------------------------------------------
// Uploads (gcs_offloader_test.go helpers)
// ---------------------------------------------------------------------------------------------

/// Go `uploadAttempt`.
#[derive(Debug, Clone, Default)]
pub(crate) struct UploadAttempt {
    pub(crate) path: String,
    pub(crate) content_type: String,
    pub(crate) data: Bytes,
    /// Go `HasDeadline` / `Remaining`.
    pub(crate) has_deadline: bool,
    pub(crate) remaining: Duration,
}

/// Go `fakeOffloader.hook`: runs at the start of every call; an error fails the call.
pub(crate) type OffloadHook =
    Arc<dyn Fn(Context, GcsUploadRequest) -> BoxFuture<'static, Result<(), Error>> + Send + Sync>;

/// Go `fakeOffloader.uri`.
pub(crate) type UriFn = Arc<dyn Fn(&GcsUploadRequest) -> String + Send + Sync>;

#[derive(Default)]
struct HookOffloaderState {
    calls: Vec<UploadAttempt>,
    closes: usize,
    in_flight: usize,
    max_in_flight: usize,
}

/// Go `fakeOffloader` (gcs_offloader_test.go): records the calls it gets; `hook` may block or
/// fail them; `uri` makes the returned URI (default `gs://fake-bucket/<path>`).
#[derive(Default)]
pub(crate) struct HookOffloader {
    pub(crate) hook: Option<OffloadHook>,
    pub(crate) uri: Option<UriFn>,
    state: Mutex<HookOffloaderState>,
}

impl HookOffloader {
    /// An offloader with `hook`.
    pub(crate) fn with_hook(hook: OffloadHook) -> HookOffloader {
        HookOffloader {
            hook: Some(hook),
            ..HookOffloader::default()
        }
    }

    /// Go `&fakeOffloader{hook: hook, uri: uri}`.
    pub(crate) fn new(hook: Option<OffloadHook>, uri: Option<UriFn>) -> HookOffloader {
        HookOffloader {
            hook,
            uri,
            ..HookOffloader::default()
        }
    }

    /// Go `uploads()`: the calls so far, ordered by path.
    pub(crate) fn uploads(&self) -> Vec<UploadAttempt> {
        let mut out = self.state.lock().unwrap().calls.clone();
        out.sort_by(|a, b| a.path.cmp(&b.path));
        out
    }

    /// Go `byPath()`.
    #[allow(dead_code)] // consumer: 7b-3 (the plugin-level gcs_offloader_test.go ports)
    pub(crate) fn by_path(&self) -> BTreeMap<String, UploadAttempt> {
        self.uploads()
            .into_iter()
            .map(|c| (c.path.clone(), c))
            .collect()
    }

    /// Go `closeCount()`.
    #[allow(dead_code)] // consumer: 7b-3 (the plugin-level gcs_offloader_test.go ports)
    pub(crate) fn close_count(&self) -> usize {
        self.state.lock().unwrap().closes
    }

    /// Go `maxConcurrent()`.
    pub(crate) fn max_concurrent(&self) -> usize {
        self.state.lock().unwrap().max_in_flight
    }
}

/// Records the call when dropped (Go's deferred record, which also covers a panicking hook).
struct RecordOnDrop<'a> {
    offloader: &'a HookOffloader,
    call: UploadAttempt,
}

impl Drop for RecordOnDrop<'_> {
    fn drop(&mut self) {
        let mut s = self
            .offloader
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        s.in_flight -= 1;
        s.calls.push(std::mem::take(&mut self.call));
    }
}

#[async_trait::async_trait]
impl GcsContentOffloader for HookOffloader {
    async fn upload_content(&self, ctx: &Context, req: GcsUploadRequest) -> Result<String, Error> {
        {
            let mut s = self.state.lock().unwrap();
            s.in_flight += 1;
            s.max_in_flight = s.max_in_flight.max(s.in_flight);
        }
        let mut record = RecordOnDrop {
            offloader: self,
            call: UploadAttempt {
                path: req.path.clone(),
                content_type: req.content_type.clone(),
                ..UploadAttempt::default()
            },
        };
        if let Some(deadline) = ctx.deadline() {
            record.call.has_deadline = true;
            record.call.remaining = deadline.saturating_duration_since(Instant::now());
        }
        if let Some(hook) = &self.hook {
            hook(ctx.clone(), req.clone()).await?;
        }
        // The data is copied after the hook, which may wait for a while: what counts is what
        // the offloader sees when it gets to use the data.
        record.call.data = req.data.clone();
        Ok(match &self.uri {
            Some(uri) => uri(&req),
            None => format!("gs://fake-bucket/{}", req.path),
        })
    }

    async fn close(&self) -> Result<(), Error> {
        self.state.lock().unwrap().closes += 1;
        Ok(())
    }
}

/// Go `imageContent(data...)`: a user message with one PNG part holding `data`.
pub(crate) fn image_content(data: &[u8]) -> Content {
    Content {
        role: "user".to_owned(),
        parts: vec![Part {
            inline_data: Some(Blob {
                mime_type: "image/png".to_owned(),
                data: Bytes::copy_from_slice(data),
                ..Blob::default()
            }),
            ..Part::default()
        }],
    }
}

/// Go `imageContents(n)`.
pub(crate) fn image_contents(n: usize) -> Vec<Content> {
    (0..n).map(|_| image_content(&[1, 2, 3])).collect()
}

/// Go `queuedRow(opts, contents...)`: the row the plugin queues for `contents` (numbered from
/// 0), with the ids that object names are made of.
pub(crate) fn queued_row(opts: &PartsOptions, contents: &[Content]) -> QueuedRow {
    let mut c = PartsCollector::new(opts.clone());
    for (i, content) in contents.iter().enumerate() {
        c.add_content(Some(content), i);
    }
    let mut row = Row::new();
    row.insert(
        "timestamp".to_owned(),
        RowValue::Timestamp(utc(2026, 3, 4, 5, 6, 7, 0)),
    );
    row.insert("event_id".to_owned(), RowValue::from("event1"));
    row.insert("trace_id".to_owned(), RowValue::from("trace1"));
    row.insert("span_id".to_owned(), RowValue::from("span1"));
    row.insert(
        "content_parts".to_owned(),
        RowValue::Parts(
            c.parts
                .iter()
                .map(
                    |p| match from_json_value(&serde_json::Value::Object(p.clone())) {
                        RowValue::Map(m) => m,
                        _ => Row::new(),
                    },
                )
                .collect(),
        ),
    );
    let mut q = QueuedRow::new(row);
    q.raw_parts = c.pending;
    q
}

/// Go `partsOptions{..., reserveBytes: bp.tryReserveUploadBytes}`.
pub(crate) fn reserve_with(
    bp: &Arc<BatchProcessor>,
) -> Option<Arc<dyn Fn(i64) -> bool + Send + Sync>> {
    let bp = Arc::clone(bp);
    Some(Arc::new(move |n| bp.try_reserve_upload_bytes(n)))
}

/// Go `bp.queue <- row`.
pub(crate) fn push_queue(bp: &BatchProcessor, row: QueuedRow) {
    bp.queue.lock().unwrap().push_back(row);
}

/// Go `len(bp.queue)`.
pub(crate) fn queue_len(bp: &BatchProcessor) -> usize {
    bp.queue.lock().unwrap().len()
}

/// Go `bp.uploadCooldownUntil.Load() != 0`.
pub(crate) fn upload_paused(bp: &BatchProcessor) -> bool {
    bp.upload_cooldown_until.lock().unwrap().is_some()
}

/// Go `bp.pendingUploadBytes.Load()`.
pub(crate) fn pending_upload_bytes(bp: &BatchProcessor) -> i64 {
    bp.pending_upload_bytes.load(Ordering::SeqCst)
}
