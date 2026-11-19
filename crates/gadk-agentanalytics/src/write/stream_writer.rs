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

//! The persistent AppendRows stream writer: per-request send/receive, the error classes
//! (`is_permanent_write_error`, `is_transient_in_band_error`, `is_ambiguous_send_error`),
//! committed-stream rotation with its cooldown and background finalization of retired streams,
//! exactly-once offsets, the request builders ([`append_rows_request`],
//! `serialized_request_size`), the committed-stream creation and the trace id
//! ([`stream_writer_trace_id`], [`stream_writer_trace_id_for_version`]).

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use arrow_array::RecordBatch;
use prost::Message as _;
use tokio::time::Instant;
use tokio_util::task::TaskTracker;

use crate::config::Config;
use crate::context::{AfterFunc, Context, ContextError};
use crate::error::{Code, Error, status_code};
use crate::storagepb::append_rows_request::{ArrowData, Rows};
use crate::storagepb::append_rows_response::Response;
use crate::storagepb::google::protobuf::Int64Value;
use crate::storagepb::{
    AppendRowsRequest, AppendRowsResponse, ArrowRecordBatch, ArrowSchema, CreateWriteStreamRequest,
    FinalizeWriteStreamRequest, WriteStream, write_stream,
};
use crate::write::arrow_batch::serialize_record;
use crate::write::gapic::{REQUEST_PARAMS_KEY, WriteClient};
use crate::write::{AppendRowsClient, code_from_rpc, is_eof};

/// The prefix of the AppendRows trace id, which names gadk (not the upstream ADK) and the
/// gadk-agentanalytics crate version.
pub const STREAM_WRITER_TRACE_ID_PREFIX: &str = "gadk-bq-logger-rust";

/// Bounds `CreateWriteStream` of a rotation and the finalization of a retired stream.
pub(crate) const DEFAULT_ROTATE_TIMEOUT: Duration = Duration::from_secs(30);

/// The trace id, computed once.
static TRACE_ID: LazyLock<String> =
    LazyLock::new(|| stream_writer_trace_id_for_version(env!("CARGO_PKG_VERSION")));

/// The AppendRows trace id: `gadk-bq-logger-rust/<CARGO_PKG_VERSION>`.
pub fn stream_writer_trace_id() -> &'static str {
    TRACE_ID.as_str()
}

/// The trace id for `version`: the prefix, and `/<version>` unless `version` is empty.
pub fn stream_writer_trace_id_for_version(version: &str) -> String {
    if version.is_empty() {
        return STREAM_WRITER_TRACE_ID_PREFIX.to_owned();
    }
    format!("{STREAM_WRITER_TRACE_ID_PREFIX}/{version}")
}

/// Builds the AppendRows request: `write_stream`, `trace_id` and `arrow_rows.writer_schema`
/// always, `arrow_rows.rows` (`serialized_record_batch`, `row_count`), `offset` only when
/// `Some` (exactly-once; offset 0 is a present, empty `Int64Value`).
// prost encodes the `rows` oneof (field 5) after `trace_id` (field 6), not in field-number
// order; the fields, their bytes and the encoded size are the same (parsers accept any field
// order).
pub fn append_rows_request(
    write_stream: &str,
    trace_id: &str,
    schema_bytes: &[u8],
    batch_bytes: &[u8],
    num_rows: i64,
    offset: Option<i64>,
) -> AppendRowsRequest {
    // The deprecated `ArrowRecordBatch.row_count` is set too.
    #[allow(deprecated)]
    let rows = ArrowRecordBatch {
        serialized_record_batch: batch_bytes.to_vec(),
        row_count: num_rows,
    };
    AppendRowsRequest {
        write_stream: write_stream.to_owned(),
        offset: offset.map(|value| Int64Value { value }),
        trace_id: trace_id.to_owned(),
        rows: Some(Rows::ArrowRows(ArrowData {
            writer_schema: Some(ArrowSchema {
                serialized_schema: schema_bytes.to_vec(),
            }),
            rows: Some(rows),
        })),
        ..AppendRowsRequest::default()
    }
}

/// Creates a `COMMITTED` stream on `projects/<p>/datasets/<d>/tables/<t>` and returns its name.
///
/// # Errors
///
/// `no write client` without a client; `failed to create committed write stream: <err>`.
pub(crate) async fn create_committed_write_stream(
    ctx: &Context,
    client: Option<&WriteClient>,
    config: &Config,
) -> Result<String, Error> {
    let Some(client) = client else {
        return Err(Error::msg("no write client"));
    };
    let parent = format!(
        "projects/{}/datasets/{}/tables/{}",
        config.project_id, config.dataset_id, config.table_name
    );
    let ws = client
        .create_write_stream(
            ctx,
            CreateWriteStreamRequest {
                parent,
                write_stream: Some(WriteStream {
                    r#type: write_stream::Type::Committed as i32,
                    ..WriteStream::default()
                }),
            },
        )
        .await
        .map_err(|e| Error::wrap("failed to create committed write stream", e))?;
    Ok(ws.name)
}

/// Whether an in-band error `code` is transient: Unavailable, ResourceExhausted, Internal,
/// DeadlineExceeded, Aborted.
pub(crate) fn is_transient_in_band_error(code: Code) -> bool {
    matches!(
        code,
        Code::Unavailable
            | Code::ResourceExhausted
            | Code::Internal
            | Code::DeadlineExceeded
            | Code::Aborted
    )
}

/// Whether a send error `code` leaves the batch's outcome ambiguous: Unavailable,
/// DeadlineExceeded, Canceled, Aborted, Internal, Unknown.
pub(crate) fn is_ambiguous_send_error(code: Code) -> bool {
    matches!(
        code,
        Code::Unavailable
            | Code::DeadlineExceeded
            | Code::Canceled
            | Code::Aborted
            | Code::Internal
            | Code::Unknown
    )
}

/// The fields of a [`StreamWriter`] guarded by its `state` mutex (tests set some of them
/// directly).
pub(crate) struct StreamState {
    /// The name of the current write stream.
    pub(crate) stream_name: String,
    /// The open AppendRows stream, if any.
    pub(crate) stream: Option<Box<dyn AppendRowsClient>>,
    /// The open stream's context (`cancel()` ends the stream).
    pub(crate) stream_ctx: Option<Context>,
    /// The stream offset can no longer be trusted (an earlier batch's outcome is unknown).
    pub(crate) offset_desynced: bool,
    /// The stream was just rotated and no request has succeeded on it yet (cleared by the first
    /// confirmed batch); a failure in that state arms the rotation cooldown.
    pub(crate) freshly_rotated: bool,
    /// The earliest instant of the next rotation; `None` means no cooldown.
    pub(crate) rotate_retry_at: Option<Instant>,
    /// How long a rotation may take (tests shorten it); zero means [`DEFAULT_ROTATE_TIMEOUT`].
    pub(crate) rotate_timeout: Duration,
    /// The clock (tests pin it); `None` reads [`Instant::now`].
    pub(crate) now: Option<Arc<dyn Fn() -> Instant + Send + Sync>>,
    /// A request was handed to `stream` and its response not yet received. Still set when
    /// `append` starts, an earlier `append` future was dropped in between, so `stream` holds an
    /// unread response and is not reused.
    pub(crate) request_in_flight: bool,
}

/// A persistent AppendRows stream on one write stream.
pub(crate) struct StreamWriter {
    /// The client rotation and stream opening use; `None` makes rotation fail with
    /// `no write client` (opening a stream is not possible).
    pub(crate) write_client: Option<WriteClient>,
    /// The serialized Arrow writer schema of every request.
    pub(crate) schema_bytes: Vec<u8>,
    /// The writer configuration.
    pub(crate) config: Config,
    /// `without_cancel` of the constructor's context (keeps its outgoing metadata).
    pub(crate) base_ctx: Context,
    /// Parent of retired-stream finalizations (`cancel()` abandons them).
    pub(crate) finalize_ctx: Context,
    /// The running finalizations of retired streams.
    pub(crate) finalizations: TaskTracker,
    /// The offset of the next request under exactly-once delivery.
    pub(crate) stream_offset: AtomicI64,
    /// The mutable stream state, behind a mutex.
    pub(crate) state: tokio::sync::Mutex<StreamState>,
}

/// A watcher that cancels the stream's context when the request's context ends, and is also
/// stopped when dropped (an `append` future dropped mid-call leaves no watcher behind to cancel
/// a reused stream).
struct StopOnDrop(AfterFunc);

impl StopOnDrop {
    /// Stops the watcher; whether it was stopped before it ran.
    fn stop(&self) -> bool {
        self.0.stop()
    }
}

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        self.0.stop();
    }
}

/// Cancels `stream_ctx` once `ctx` ends.
fn cancel_stream_after(ctx: &Context, stream_ctx: &Context) -> StopOnDrop {
    let stream_ctx = stream_ctx.clone();
    StopOnDrop(ctx.after_func(move || stream_ctx.cancel()))
}

/// The error of a context known to have ended.
fn context_error(ctx: &Context) -> Error {
    Error::Context(ctx.err().unwrap_or(ContextError::Canceled))
}

/// `now + d` (saturating far in the future instead of overflowing).
fn instant_after(now: Instant, d: Duration) -> Instant {
    now.checked_add(d)
        .or_else(|| now.checked_add(Duration::from_secs(1 << 32)))
        .unwrap_or(now)
}

/// The error for a rotation attempted during its cooldown, naming the instant (as local
/// wall-clock time, RFC 3339 with nanoseconds) the cooldown ends.
fn rotation_cooldown_error(at: Instant) -> Error {
    let now = Instant::now();
    let wall = chrono::Local::now();
    let at_wall = match at.checked_duration_since(now) {
        Some(ahead) => chrono::TimeDelta::from_std(ahead)
            .ok()
            .and_then(|d| wall.checked_add_signed(d)),
        None => chrono::TimeDelta::from_std(now.saturating_duration_since(at))
            .ok()
            .and_then(|d| wall.checked_sub_signed(d)),
    }
    .unwrap_or(wall);
    Error::msg(format!(
        "committed write stream rotation in cooldown until {}",
        gadk_platform::go_format_rfc3339_nano(&at_wall)
    ))
}

/// `d` in whole nanoseconds, saturating at `i64::MAX`.
fn go_nanos(d: Duration) -> i64 {
    i64::try_from(d.as_nanos()).unwrap_or(i64::MAX)
}

/// The `google.rpc.Status` of an in-band `AppendRowsResponse.error`, if any.
fn in_band_error(res: &AppendRowsResponse) -> Option<&crate::storagepb::google::rpc::Status> {
    match &res.response {
        Some(Response::Error(status)) => Some(status),
        _ => None,
    }
}

impl StreamWriter {
    /// Creates a writer for `stream_name` over `client` (which may be `None`).
    pub(crate) fn new(
        ctx: &Context,
        client: Option<WriteClient>,
        stream_name: &str,
        schema_bytes: Vec<u8>,
        config: Config,
    ) -> StreamWriter {
        let base_ctx = ctx.without_cancel();
        let finalize_ctx = base_ctx.with_cancel();
        // Closed from the start, so `wait` resolves whenever no finalization runs (a closed
        // tracker still accepts tasks).
        let finalizations = TaskTracker::new();
        finalizations.close();
        StreamWriter {
            write_client: client,
            schema_bytes,
            config,
            base_ctx,
            finalize_ctx,
            finalizations,
            stream_offset: AtomicI64::new(0),
            state: tokio::sync::Mutex::new(StreamState {
                stream_name: stream_name.to_owned(),
                stream: None,
                stream_ctx: None,
                offset_desynced: false,
                freshly_rotated: false,
                rotate_retry_at: None,
                rotate_timeout: DEFAULT_ROTATE_TIMEOUT,
                now: None,
                request_in_flight: false,
            }),
        }
    }

    /// Sends `record` (as `batch_bytes` when given, else serialized once and reused) and returns
    /// BigQuery's response, retrying per `config.retry_config`; `rotated_in_batch` is the
    /// rotation budget the chunks of one batch share.
    ///
    /// # Errors
    ///
    /// `max retries exceeded: …`, `failed to open append rows stream: …`, `failed to send row
    /// batch: …`, `failed to receive response for row batch: …`, the exactly-once rotation
    /// errors, the context's error.
    pub(crate) async fn append(
        &self,
        ctx: &Context,
        record: &RecordBatch,
        batch_bytes: Option<Vec<u8>>,
        rotated_in_batch: &mut bool,
    ) -> Result<AppendRowsResponse, Error> {
        let mut state = self.state.lock().await;
        if state.request_in_flight {
            // An earlier `append` future was dropped between sending its request and receiving
            // the response: the stream would answer the next request with that response.
            // Replace the stream and, as for a batch whose outcome stayed unknown, distrust the
            // offset.
            Self::reset_stream(&mut state);
            if self.config.exactly_once_delivery {
                state.offset_desynced = true;
                if state.freshly_rotated {
                    self.arm_rotation_cooldown(&mut state);
                }
            }
        }
        let mut had_ambiguous_send = false;
        if self.config.exactly_once_delivery && state.offset_desynced {
            let now = Self::now_or_default(&state);
            if let Some(at) = state.rotate_retry_at
                && now < at
            {
                return Err(rotation_cooldown_error(at));
            }
            // Not reachable today: every place that sets offset_desynced also fails the chunk,
            // and the processor stops at the first failed chunk; the check keeps a batch to one
            // rotation should that change.
            if *rotated_in_batch {
                return Err(Error::msg(
                    "committed write stream already rotated in batch",
                ));
            }
            *rotated_in_batch = true;
            if let Err(rerr) = self.rotate_committed_stream(&mut state, ctx).await {
                return Err(Error::wrap(
                    "committed stream out of sync and not replaced",
                    rerr,
                ));
            }
            state.offset_desynced = false;
        }

        let res = self
            .append_attempts(
                &mut state,
                ctx,
                record,
                batch_bytes,
                rotated_in_batch,
                &mut had_ambiguous_send,
            )
            .await;

        // A batch whose outcome stayed unknown leaves the offset untrusted.
        if self.config.exactly_once_delivery && had_ambiguous_send {
            let failed = match &res {
                Err(_) => true,
                Ok(r) => in_band_error(r).is_some() || !r.row_errors.is_empty(),
            };
            if failed {
                state.offset_desynced = true;
                if state.freshly_rotated {
                    self.arm_rotation_cooldown(&mut state);
                }
            }
        }
        res
    }

    /// The retry loop of `append` (after the exactly-once prologue).
    #[allow(clippy::too_many_lines)]
    async fn append_attempts(
        &self,
        state: &mut StreamState,
        ctx: &Context,
        record: &RecordBatch,
        mut batch_bytes: Option<Vec<u8>>,
        rotated_in_batch: &mut bool,
        had_ambiguous_send: &mut bool,
    ) -> Result<AppendRowsResponse, Error> {
        let exactly_once = self.config.exactly_once_delivery;
        let retry = self.config.retry_config;
        let max_retries = retry.max_retries;
        let max_delay = go_nanos(retry.max_delay);
        let mut delay = go_nanos(retry.initial_delay);
        let num_rows = i64::try_from(record.num_rows()).unwrap_or(i64::MAX);
        let mut last_err: Option<Error> = None;
        let mut fresh_stream_retried = false;
        // A stream already open was opened for an earlier request: its stream-closing statuses
        // never fail the batch fast.
        let mut reused_stream = state.stream.is_some();

        let mut attempt: i64 = -1;
        loop {
            attempt += 1;
            if attempt > max_retries {
                break;
            }
            if attempt > 0 {
                let pause = Duration::from_nanos(u64::try_from(delay).unwrap_or(0));
                tokio::select! {
                    biased;
                    () = ctx.done() => return Err(context_error(ctx)),
                    () = tokio::time::sleep(pause) => {
                        // Multiply the delay (saturating float-to-int conversion), capped at
                        // the maximum delay.
                        delay = (delay as f64 * retry.multiplier) as i64;
                        if delay > max_delay {
                            delay = max_delay;
                        }
                    }
                }
            }

            if let Some(err) = ctx.err() {
                return Err(Error::Context(err));
            }

            if state.stream.is_some()
                && state
                    .stream_ctx
                    .as_ref()
                    .is_some_and(|sc| sc.err().is_some())
            {
                Self::reset_stream(state);
            }

            if state.stream.is_none() {
                reused_stream = false;
                let mut base_ctx = if self.base_ctx.err().is_some() {
                    ctx.without_cancel()
                } else {
                    self.base_ctx.clone()
                };
                if !state.stream_name.is_empty() {
                    // URL-encoded like the generated client's routing headers: gRPC rejects
                    // non-ASCII metadata values, and table names may contain Unicode.
                    let routing = format!(
                        "write_stream={}",
                        gadk_platform::go_query_escape(&state.stream_name)
                    );
                    base_ctx = base_ctx.append_outgoing_metadata(&[(REQUEST_PARAMS_KEY, &routing)]);
                }
                let stream_ctx = base_ctx.with_cancel();
                let stop_open = cancel_stream_after(ctx, &stream_ctx);
                let opened = match &self.write_client {
                    Some(client) => client.append_rows(&stream_ctx).await,
                    // Without a client the open fails with `no write client`.
                    None => Err(Error::msg("no write client")),
                };
                if !stop_open.stop() {
                    stream_ctx.cancel();
                    let err = match opened {
                        Err(err) => err,
                        Ok(_) => context_error(ctx),
                    };
                    last_err = Some(Error::wrap("failed to open append rows stream", err));
                    continue;
                }
                match opened {
                    Err(err) => {
                        stream_ctx.cancel();
                        let permanent = self.is_permanent_write_error(status_code(&err));
                        let err = Error::wrap("failed to open append rows stream", err);
                        if permanent {
                            return Err(err);
                        }
                        last_err = Some(err);
                        continue;
                    }
                    Ok(stream) => {
                        state.stream = Some(stream);
                        state.stream_ctx = Some(stream_ctx);
                    }
                }
            }

            if batch_bytes.is_none() {
                batch_bytes = serialize_record(record, self.config.logger.as_deref());
                if batch_bytes.is_none() {
                    last_err = Some(Error::msg("failed to serialize record batch"));
                    continue;
                }
            }

            let req = self.new_append_rows_request_locked(
                state,
                batch_bytes.as_deref().unwrap_or_default(),
                num_rows,
            );

            let stop_call = state
                .stream_ctx
                .as_ref()
                .map(|sc| cancel_stream_after(ctx, sc));

            let prev_rotated = *rotated_in_batch;
            let Some(stream) = state.stream.as_mut() else {
                last_err = Some(Error::msg("no append rows stream"));
                continue;
            };
            state.request_in_flight = true;
            if let Err(mut send_err) = stream.send(req).await {
                // An error not generated by the client is reported as EOF here, with the real
                // status left for recv; drain until it surfaces, releasing the stream. Wrapped
                // errors match too.
                let send_eof = is_eof(&send_err);
                if send_eof {
                    loop {
                        match stream.recv().await {
                            Ok(_) => {}
                            Err(rerr) => {
                                if !is_eof(&rerr) {
                                    send_err = rerr;
                                }
                                break;
                            }
                        }
                    }
                }
                if let Some(stop) = &stop_call {
                    stop.stop();
                }
                Self::reset_stream(state);
                let code = status_code(&send_err);
                if exactly_once {
                    if let Some(res) = self
                        .handle_exactly_once_error(
                            state,
                            ctx,
                            code,
                            num_rows,
                            had_ambiguous_send,
                            rotated_in_batch,
                            false,
                        )
                        .await?
                    {
                        return Ok(res);
                    }
                    if !prev_rotated && *rotated_in_batch {
                        attempt -= 1;
                    }
                }
                let err = Error::wrap("failed to send row batch", send_err);
                if !reused_stream && self.is_permanent_write_error(code) {
                    if !send_eof || fresh_stream_retried {
                        return Err(err);
                    }
                    fresh_stream_retried = true;
                }
                last_err = Some(err);
                continue;
            }

            let recv = stream.recv().await;
            state.request_in_flight = false;
            if let Some(stop) = &stop_call {
                stop.stop();
            }
            drop(stop_call);
            let res = match recv {
                Ok(res) => res,
                Err(recv_err) => {
                    Self::reset_stream(state);
                    let code = status_code(&recv_err);
                    if exactly_once {
                        if let Some(res) = self
                            .handle_exactly_once_error(
                                state,
                                ctx,
                                code,
                                num_rows,
                                had_ambiguous_send,
                                rotated_in_batch,
                                true,
                            )
                            .await?
                        {
                            return Ok(res);
                        }
                        if !prev_rotated && *rotated_in_batch {
                            attempt -= 1;
                        }
                    }
                    let err = Error::wrap("failed to receive response for row batch", recv_err);
                    if !reused_stream && self.is_permanent_write_error(code) {
                        if fresh_stream_retried {
                            return Err(err);
                        }
                        fresh_stream_retried = true;
                    }
                    last_err = Some(err);
                    continue;
                }
            };

            if exactly_once && let Some(in_band) = in_band_error(&res) {
                // An in-band error is the server's rejection of this attempt: it does not
                // itself make the outcome ambiguous. A number with no known status code matches
                // none of the classes (Unknown with send_succeeded false does the same).
                let code = code_from_rpc(in_band.code).unwrap_or(Code::Unknown);
                let message = in_band.message.clone();
                if let Some(ok) = self
                    .handle_exactly_once_error(
                        state,
                        ctx,
                        code,
                        num_rows,
                        had_ambiguous_send,
                        rotated_in_batch,
                        false,
                    )
                    .await?
                {
                    return Ok(ok);
                }
                if !prev_rotated && *rotated_in_batch {
                    attempt -= 1;
                }
                if matches!(
                    code,
                    Code::AlreadyExists | Code::OutOfRange | Code::NotFound
                ) {
                    last_err = Some(Error::msg(format!(
                        "batch append error from BigQuery: {message}"
                    )));
                    continue;
                }
            }

            // A transient status in the response is resent like the same status on the
            // stream; once the retries are used up, the response is returned as it is.
            if let Some(in_band) = in_band_error(&res)
                && attempt < max_retries
                && code_from_rpc(in_band.code).is_some_and(is_transient_in_band_error)
            {
                last_err = Some(Error::msg(format!(
                    "batch append error from BigQuery: {}",
                    in_band.message
                )));
                continue;
            }

            if exactly_once && in_band_error(&res).is_none() && res.row_errors.is_empty() {
                self.stream_offset.fetch_add(num_rows, Ordering::SeqCst);
                *had_ambiguous_send = false;
                state.offset_desynced = false;
                state.freshly_rotated = false;
            }

            return Ok(res);
        }

        Err(match last_err {
            Some(err) => Error::wrap("max retries exceeded", err),
            None => Error::msg("max retries exceeded"),
        })
    }

    /// The request `append` sends ([`append_rows_request`] with this writer's stream, trace id,
    /// schema and, with exactly-once, `stream_offset`).
    pub(crate) fn new_append_rows_request_locked(
        &self,
        state: &StreamState,
        batch_bytes: &[u8],
        num_rows: i64,
    ) -> AppendRowsRequest {
        let offset = self
            .config
            .exactly_once_delivery
            .then(|| self.stream_offset.load(Ordering::SeqCst));
        append_rows_request(
            &state.stream_name,
            stream_writer_trace_id(),
            &self.schema_bytes,
            batch_bytes,
            num_rows,
            offset,
        )
    }

    /// The encoded length of the request `append` would send.
    pub(crate) async fn serialized_request_size(&self, batch_bytes: &[u8], num_rows: i64) -> usize {
        let req = {
            let state = self.state.lock().await;
            self.new_append_rows_request_locked(&state, batch_bytes, num_rows)
        };
        req.encoded_len()
    }

    /// Whether `code` is a permanent write error: PermissionDenied, InvalidArgument,
    /// Unauthenticated, FailedPrecondition, Unimplemented; NotFound unless exactly-once.
    pub(crate) fn is_permanent_write_error(&self, code: Code) -> bool {
        match code {
            Code::PermissionDenied
            | Code::InvalidArgument
            | Code::Unauthenticated
            | Code::FailedPrecondition
            | Code::Unimplemented => true,
            Code::NotFound => !self.config.exactly_once_delivery,
            _ => false,
        }
    }

    /// Whether exactly-once delivery has an untrusted offset and a rotation cooldown still
    /// running.
    pub(crate) async fn is_rotation_in_cooldown(&self) -> bool {
        let state = self.state.lock().await;
        let now = Self::now_or_default(&state);
        self.config.exactly_once_delivery
            && state.offset_desynced
            && state.rotate_retry_at.is_some_and(|at| now < at)
    }

    /// Closes the open stream (if any) and cancels its context.
    pub(crate) async fn close_active_stream(&self) {
        let mut state = self.state.lock().await;
        Self::reset_stream(&mut state);
    }

    /// Abandons the running finalizations of retired streams.
    pub(crate) fn cancel_finalizations(&self) {
        self.finalize_ctx.cancel();
    }

    /// Waits until the running finalizations of retired streams have finished.
    pub(crate) async fn wait_finalizations(&self) {
        self.finalizations.close();
        self.finalizations.wait().await;
    }

    /// The current stream's name (taken), or none.
    pub(crate) async fn streams_to_finalize(&self) -> Vec<String> {
        let mut state = self.state.lock().await;
        if state.stream_name.is_empty() {
            return Vec::new();
        }
        vec![std::mem::take(&mut state.stream_name)]
    }

    /// The current time: the pinned clock if set, else [`Instant::now`].
    pub(crate) fn now_or_default(state: &StreamState) -> Instant {
        match &state.now {
            Some(now) => now(),
            None => Instant::now(),
        }
    }

    /// The rotation timeout, or [`DEFAULT_ROTATE_TIMEOUT`] when unset.
    pub(crate) fn rotate_timeout_or_default(state: &StreamState) -> Duration {
        if state.rotate_timeout > Duration::ZERO {
            state.rotate_timeout
        } else {
            DEFAULT_ROTATE_TIMEOUT
        }
    }

    /// Closes and cancels the open stream and clears the in-flight flag.
    pub(crate) fn reset_stream(state: &mut StreamState) {
        state.request_in_flight = false;
        if let Some(mut stream) = state.stream.take() {
            stream.close_send();
        }
        if let Some(stream_ctx) = state.stream_ctx.take() {
            stream_ctx.cancel();
        }
    }

    /// Rotates to a fresh committed stream (caller holds `state`).
    ///
    /// # Errors
    ///
    /// `committed write stream rotation in cooldown until <RFC 3339>`, or the
    /// `CreateWriteStream` error.
    pub(crate) async fn rotate_committed_stream(
        &self,
        state: &mut StreamState,
        ctx: &Context,
    ) -> Result<(), Error> {
        Self::reset_stream(state);
        let now = Self::now_or_default(state);
        if let Some(at) = state.rotate_retry_at
            && now < at
        {
            return Err(rotation_cooldown_error(at));
        }
        let timeout = Self::rotate_timeout_or_default(state);
        // The remaining time is under `min(timeout, shutdown_timeout / 2)` (a passed deadline
        // counts as starved).
        let starved = ctx.deadline().is_some_and(|deadline| {
            deadline
                .checked_duration_since(Instant::now())
                .is_none_or(|left| left < timeout.min(self.config.shutdown_timeout / 2))
        });
        let rotate_ctx = ctx.with_timeout(timeout);
        let created =
            create_committed_write_stream(&rotate_ctx, self.write_client.as_ref(), &self.config)
                .await;
        rotate_ctx.cancel();
        let new_stream = match created {
            Ok(name) => name,
            Err(err) => {
                if !starved || ctx.err().is_none() {
                    state.rotate_retry_at =
                        Some(instant_after(Self::now_or_default(state), timeout));
                }
                return Err(err);
            }
        };
        state.rotate_retry_at = None;
        if !state.stream_name.is_empty() {
            let retired = state.stream_name.clone();
            let parent = self.finalize_ctx.clone();
            let logger = self.config.logger.clone();
            let client = self.write_client.clone();
            self.finalizations.spawn(async move {
                let Some(client) = client else {
                    return;
                };
                let fctx = parent.with_timeout(timeout);
                let res = client
                    .finalize_write_stream(
                        &fctx,
                        FinalizeWriteStreamRequest {
                            name: retired.clone(),
                        },
                    )
                    .await;
                fctx.cancel();
                if let (Err(err), Some(logger)) = (res, logger) {
                    logger.printf(format_args!(
                        "agentanalytics: failed to finalize write stream {retired}: {err}"
                    ));
                }
            });
        }
        state.stream_name = new_stream;
        self.stream_offset.store(0, Ordering::SeqCst);
        state.freshly_rotated = true;
        Ok(())
    }

    /// Handles an exactly-once error `code` of a batch of `num_rows` rows (caller holds `state`):
    /// `Ok(Some(response))` means the batch is confirmed, `Ok(None)` retries, `Err` is a
    /// rotation error.
    ///
    /// # Errors
    ///
    /// `batch outcome unknown on stream (…)…`, `committed write stream already rotated (…)`,
    /// `failed to rotate committed write stream after …: …`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn handle_exactly_once_error(
        &self,
        state: &mut StreamState,
        ctx: &Context,
        code: Code,
        num_rows: i64,
        had_ambiguous_send: &mut bool,
        rotated_in_batch: &mut bool,
        send_succeeded: bool,
    ) -> Result<Option<AppendRowsResponse>, Error> {
        match code {
            Code::AlreadyExists | Code::OutOfRange | Code::NotFound => {
                if code == Code::AlreadyExists && *had_ambiguous_send {
                    self.stream_offset.fetch_add(num_rows, Ordering::SeqCst);
                    *had_ambiguous_send = false;
                    state.offset_desynced = false;
                    state.freshly_rotated = false;
                    return Ok(Some(AppendRowsResponse::default()));
                }
                if *had_ambiguous_send {
                    *had_ambiguous_send = false;
                    state.offset_desynced = true;
                    if state.freshly_rotated {
                        self.arm_rotation_cooldown(state);
                    }
                    return Err(Error::msg(format!(
                        "batch outcome unknown on stream ({code}); dropped to prevent duplicate \
                         delivery"
                    )));
                }
                if *rotated_in_batch || state.freshly_rotated {
                    state.offset_desynced = true;
                    if state.freshly_rotated {
                        self.arm_rotation_cooldown(state);
                    }
                    return Err(Error::msg(format!(
                        "committed write stream already rotated ({code})"
                    )));
                }
                *rotated_in_batch = true;
                if let Err(err) = self.rotate_committed_stream(state, ctx).await {
                    state.offset_desynced = true;
                    return Err(Error::wrap(
                        format!("failed to rotate committed write stream after {code}"),
                        err,
                    ));
                }
                state.offset_desynced = false;
                Ok(None)
            }
            _ => {
                if send_succeeded && is_ambiguous_send_error(code) {
                    *had_ambiguous_send = true;
                }
                Ok(None)
            }
        }
    }

    /// Starts the rotation cooldown: the next rotation is allowed one rotation timeout from now.
    fn arm_rotation_cooldown(&self, state: &mut StreamState) {
        let now = Self::now_or_default(state);
        state.rotate_retry_at = Some(instant_after(now, Self::rotate_timeout_or_default(state)));
    }
}

#[cfg(test)]
pub(crate) mod tests {
    //! Unit tests, and the test doubles the batch processor's ported Go tests reuse
    //! ([`MockStream`] = Go `mockStream`, [`scripted`] = Go `scriptedCommittedStreamServer`).

    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use std::path::PathBuf;
    use std::sync::Mutex;
    use std::sync::atomic::AtomicUsize;

    use pretty_assertions::assert_eq;
    use serde_json::Value;

    use super::*;
    use crate::bigquery::fake::RecordingLogger;
    use crate::config::{Logger, RetryConfig};
    use crate::context::Metadata;
    use crate::row::{Row, RowValue};
    use crate::schema::{arrow_schema, serialized_arrow_schema};
    use crate::storagepb::FinalizeWriteStreamResponse;
    use crate::storagepb::google::rpc;
    use crate::testutil::fake_write_server::{AppendRowsServerStream, FakeWriteServer};
    use crate::write::arrow_batch::build_record_batch;
    use crate::write::eof;

    const PARENT: &str = "projects/test-proj/datasets/test-ds/tables/agent_events";

    fn committed(n: usize) -> String {
        format!("{PARENT}/streams/committed-{n}")
    }

    fn row(event_type: &str) -> Row {
        let mut row = Row::new();
        row.insert("event_type".to_owned(), RowValue::from(event_type));
        row
    }

    fn record(event_types: &[&str]) -> RecordBatch {
        let rows: Vec<Row> = event_types.iter().map(|e| row(e)).collect();
        build_record_batch(&rows, &arrow_schema()).unwrap()
    }

    /// Go's `exactlyOnceTestConfig()` with `newErrorClassificationProcessor`'s retries
    /// (3 retries, 1 ms → 2 ms, ×1) and a recording logger.
    fn test_config(exactly_once: bool, logger: &Arc<RecordingLogger>) -> Config {
        Config {
            project_id: "test-proj".to_owned(),
            dataset_id: "test-ds".to_owned(),
            table_name: "agent_events".to_owned(),
            exactly_once_delivery: exactly_once,
            retry_config: RetryConfig {
                max_retries: 3,
                initial_delay: Duration::from_millis(1),
                max_delay: Duration::from_millis(2),
                multiplier: 1.0,
            },
            logger: Some(Arc::clone(logger) as Arc<dyn Logger>),
            ..Config::default()
        }
    }

    fn writer_on(
        ctx: &Context,
        client: Option<WriteClient>,
        stream_name: &str,
        config: Config,
    ) -> StreamWriter {
        StreamWriter::new(
            ctx,
            client,
            stream_name,
            serialized_arrow_schema().unwrap(),
            config,
        )
    }

    pub(crate) fn tonic_status(code: Code, message: &str) -> tonic::Status {
        tonic::Status::new(tonic::Code::from_i32(code.as_u32() as i32), message)
    }

    pub(crate) fn in_band(code: Code, message: &str) -> AppendRowsResponse {
        AppendRowsResponse {
            response: Some(Response::Error(rpc::Status {
                code: code.as_u32() as i32,
                message: message.to_owned(),
                ..rpc::Status::default()
            })),
            ..AppendRowsResponse::default()
        }
    }

    /// One AppendRows request as Go's `scriptedCommittedStreamServer` records it: the stream
    /// name and the offset (`-1` without one).
    pub(crate) type Delivered = Arc<Mutex<Vec<(String, i64)>>>;

    /// Go's `scriptedCommittedStreamServer.AppendRows`: every request is recorded, then
    /// `on_append(attempt, req)` answers it or ends the stream with its status.
    pub(crate) fn scripted(
        server: &FakeWriteServer,
        on_append: impl Fn(usize, &AppendRowsRequest) -> Result<AppendRowsResponse, tonic::Status>
        + Send
        + Sync
        + 'static,
    ) -> Delivered {
        let delivered: Delivered = Arc::default();
        let on_append = Arc::new(on_append);
        let log = Arc::clone(&delivered);
        server.on_append_rows(Arc::new(move |mut stream: AppendRowsServerStream| {
            let on_append = Arc::clone(&on_append);
            let log = Arc::clone(&log);
            Box::pin(async move {
                while let Some(req) = stream.recv().await? {
                    let attempt = {
                        let mut log = log.lock().unwrap();
                        log.push((req.write_stream.clone(), req.offset.map_or(-1, |o| o.value)));
                        log.len()
                    };
                    let res = on_append(attempt, &req)?;
                    stream.send(res).await?;
                }
                Ok(())
            })
        }));
        delivered
    }

    /// The scripted failures of [`MockStream`] (`Error` is not `Clone`).
    #[derive(Debug, Clone)]
    pub(crate) enum Fail {
        Eof,
        WrappedEof(&'static str),
        Status(Code, &'static str),
        Msg(&'static str),
    }

    impl Fail {
        pub(crate) fn error(&self) -> Error {
            match self {
                Fail::Eof => eof(),
                Fail::WrappedEof(prefix) => Error::wrap(*prefix, eof()),
                Fail::Status(code, message) => Error::Status {
                    code: *code,
                    message: (*message).to_owned(),
                },
                Fail::Msg(text) => Error::msg(*text),
            }
        }
    }

    #[derive(Default)]
    pub(crate) struct MockCounts {
        pub(crate) sends: AtomicUsize,
        pub(crate) recvs: AtomicUsize,
        pub(crate) sent: Mutex<Vec<AppendRowsRequest>>,
    }

    /// Go's `mockStream`.
    #[derive(Default)]
    pub(crate) struct MockStream {
        pub(crate) send_err: Option<Fail>,
        pub(crate) recv_seq: Vec<Result<AppendRowsResponse, Fail>>,
        pub(crate) recv_res: Option<AppendRowsResponse>,
        pub(crate) recv_err: Option<Fail>,
        pub(crate) counts: Arc<MockCounts>,
    }

    #[async_trait::async_trait]
    impl AppendRowsClient for MockStream {
        async fn send(&mut self, req: AppendRowsRequest) -> Result<(), Error> {
            self.counts.sends.fetch_add(1, Ordering::SeqCst);
            self.counts.sent.lock().unwrap().push(req);
            match &self.send_err {
                Some(fail) => Err(fail.error()),
                None => Ok(()),
            }
        }

        async fn recv(&mut self) -> Result<AppendRowsResponse, Error> {
            let n = self.counts.recvs.fetch_add(1, Ordering::SeqCst) + 1;
            if let Some(next) = self.recv_seq.get(n - 1) {
                return next.clone().map_err(|f| f.error());
            }
            if self.recv_err.is_none() && n > 1000 {
                return Err(Error::msg(format!(
                    "mockStream.Recv called {n} times with no terminal error set"
                )));
            }
            match (&self.recv_res, &self.recv_err) {
                (_, Some(fail)) => Err(fail.error()),
                (Some(res), None) => Ok(res.clone()),
                (None, None) => Err(eof()),
            }
        }

        fn close_send(&mut self) {}
    }

    fn status_of(err: &Error) -> Option<Code> {
        match err.find(&mut |e| matches!(e, Error::Status { .. })) {
            Some(Error::Status { code, .. }) => Some(*code),
            _ => None,
        }
    }

    const ALL_CODES: [Code; 17] = [
        Code::Ok,
        Code::Canceled,
        Code::Unknown,
        Code::InvalidArgument,
        Code::DeadlineExceeded,
        Code::NotFound,
        Code::AlreadyExists,
        Code::PermissionDenied,
        Code::ResourceExhausted,
        Code::FailedPrecondition,
        Code::Aborted,
        Code::OutOfRange,
        Code::Unimplemented,
        Code::Internal,
        Code::Unavailable,
        Code::DataLoss,
        Code::Unauthenticated,
    ];

    #[test]
    fn trace_ids_follow_go() {
        // Go TestStreamWriterTraceID, with gadk's prefix (ledgered).
        assert_eq!(
            stream_writer_trace_id_for_version(""),
            "gadk-bq-logger-rust"
        );
        assert_eq!(
            stream_writer_trace_id_for_version("2.6.0"),
            "gadk-bq-logger-rust/2.6.0"
        );
        assert!(stream_writer_trace_id().starts_with(STREAM_WRITER_TRACE_ID_PREFIX));
        assert_eq!(
            stream_writer_trace_id(),
            stream_writer_trace_id_for_version(env!("CARGO_PKG_VERSION"))
        );
    }

    #[tokio::test]
    async fn error_classes_match_go() {
        let logger = Arc::new(RecordingLogger::default());
        let at_least_once = writer_on(
            &Context::background(),
            None,
            "",
            test_config(false, &logger),
        );
        let exactly_once = writer_on(&Context::background(), None, "", test_config(true, &logger));
        let permanent = [
            Code::PermissionDenied,
            Code::InvalidArgument,
            Code::Unauthenticated,
            Code::FailedPrecondition,
            Code::Unimplemented,
        ];
        let transient = [
            Code::Unavailable,
            Code::ResourceExhausted,
            Code::Internal,
            Code::DeadlineExceeded,
            Code::Aborted,
        ];
        let ambiguous = [
            Code::Unavailable,
            Code::DeadlineExceeded,
            Code::Canceled,
            Code::Aborted,
            Code::Internal,
            Code::Unknown,
        ];
        for code in ALL_CODES {
            assert_eq!(
                at_least_once.is_permanent_write_error(code),
                permanent.contains(&code) || code == Code::NotFound,
                "{code}"
            );
            assert_eq!(
                exactly_once.is_permanent_write_error(code),
                permanent.contains(&code),
                "{code}"
            );
            assert_eq!(
                is_transient_in_band_error(code),
                transient.contains(&code),
                "{code}"
            );
            assert_eq!(
                is_ambiguous_send_error(code),
                ambiguous.contains(&code),
                "{code}"
            );
        }
    }

    // Go TestStreamWriter_SendError (through `append` instead of `writeBatch`).
    #[tokio::test]
    async fn send_errors_drain_the_status_and_reset_the_stream() {
        struct Case {
            name: &'static str,
            send_err: Fail,
            recv_err: Option<Fail>,
            recv_seq: usize,
            want_status: Option<Code>,
            want_msg: &'static str,
            want_recvs: usize,
        }
        let cases = [
            Case {
                name: "server-side termination reports the status from Recv",
                send_err: Fail::Eof,
                recv_err: Some(Fail::Status(
                    Code::Unavailable,
                    "the connection is draining",
                )),
                recv_seq: 0,
                want_status: Some(Code::Unavailable),
                want_msg: "the connection is draining",
                want_recvs: 1,
            },
            Case {
                name: "buffered responses are drained until the status surfaces",
                send_err: Fail::Eof,
                recv_err: Some(Fail::Status(
                    Code::ResourceExhausted,
                    "Exceeds 'AppendRows throughput' quota",
                )),
                recv_seq: 3,
                want_status: Some(Code::ResourceExhausted),
                want_msg: "AppendRows throughput",
                want_recvs: 4,
            },
            Case {
                name: "a termination without a status is drained exactly once",
                send_err: Fail::Eof,
                recv_err: Some(Fail::Eof),
                recv_seq: 0,
                want_status: None,
                want_msg: "EOF",
                want_recvs: 1,
            },
            Case {
                name: "a wrapped io.EOF still drains and keeps its context",
                send_err: Fail::WrappedEof("send aborted"),
                recv_err: Some(Fail::Eof),
                recv_seq: 0,
                want_status: None,
                want_msg: "send aborted",
                want_recvs: 1,
            },
            Case {
                name: "a wrapped io.EOF still yields to the recovered status",
                send_err: Fail::WrappedEof("send aborted"),
                recv_err: Some(Fail::Status(
                    Code::ResourceExhausted,
                    "Exceeds 'AppendRows throughput' quota",
                )),
                recv_seq: 0,
                want_status: Some(Code::ResourceExhausted),
                want_msg: "AppendRows throughput",
                want_recvs: 1,
            },
            Case {
                name: "client-side error is reported as is",
                send_err: Fail::Msg("send failed"),
                recv_err: None,
                recv_seq: 0,
                want_status: None,
                want_msg: "send failed",
                want_recvs: 0,
            },
        ];
        for case in cases {
            let logger = Arc::new(RecordingLogger::default());
            let mut config = test_config(false, &logger);
            config.retry_config.max_retries = 0;
            let writer = writer_on(&Context::background(), None, "test_stream", config);
            let counts = Arc::new(MockCounts::default());
            writer.state.lock().await.stream = Some(Box::new(MockStream {
                send_err: Some(case.send_err.clone()),
                recv_seq: vec![Ok(AppendRowsResponse::default()); case.recv_seq],
                recv_err: case.recv_err.clone(),
                counts: Arc::clone(&counts),
                ..MockStream::default()
            }));
            let err = writer
                .append(&Context::background(), &record(&["X"]), None, &mut false)
                .await
                .unwrap_err();
            let text = err.to_string();
            let name = case.name;
            assert!(
                text.starts_with("max retries exceeded: failed to send row batch: "),
                "{name}: {text}"
            );
            assert!(text.contains(case.want_msg), "{name}: {text}");
            assert_eq!(
                counts.recvs.load(Ordering::SeqCst),
                case.want_recvs,
                "{name}"
            );
            assert_eq!(counts.sends.load(Ordering::SeqCst), 1, "{name}");
            assert!(writer.state.lock().await.stream.is_none(), "{name}");
            assert_eq!(status_of(&err), case.want_status, "{name}: {text}");
        }
    }

    // Go TestStreamWriter_RetryOpensAFreshStream.
    #[tokio::test(start_paused = true)]
    async fn retry_opens_a_fresh_stream() {
        let server = FakeWriteServer::new();
        let logger = Arc::new(RecordingLogger::default());
        let mut config = test_config(false, &logger);
        config.retry_config.max_retries = 1;
        let writer = writer_on(
            &Context::background(),
            Some(server.client().unwrap()),
            "projects/p/datasets/d/tables/t/_default",
            config,
        );
        let counts = Arc::new(MockCounts::default());
        writer.state.lock().await.stream = Some(Box::new(MockStream {
            send_err: Some(Fail::Eof),
            recv_err: Some(Fail::Eof),
            counts: Arc::clone(&counts),
            ..MockStream::default()
        }));
        writer
            .append(
                &Context::background(),
                &record(&["retry_probe"]),
                None,
                &mut false,
            )
            .await
            .unwrap();
        assert_eq!(counts.sends.load(Ordering::SeqCst), 1);
        assert_eq!(server.streams_opened(), 1);
        assert_eq!(server.append_requests().len(), 1);
    }

    /// A writer over `server` as Go's `newErrorClassificationProcessor` builds it: the
    /// `_default` stream, or with exactly-once a committed stream created first.
    async fn classification_writer(
        server: &FakeWriteServer,
        exactly_once: bool,
        logger: &Arc<RecordingLogger>,
    ) -> StreamWriter {
        let client = server.client().unwrap();
        let config = test_config(exactly_once, logger);
        let stream_name = if exactly_once {
            create_committed_write_stream(&Context::background(), Some(&client), &config)
                .await
                .unwrap()
        } else {
            format!("{PARENT}/_default")
        };
        writer_on(&Context::background(), Some(client), &stream_name, config)
    }

    // Go TestStreamWriter_PermanentStatusFailsFast.
    #[tokio::test(start_paused = true)]
    async fn permanent_statuses_fail_fast_after_one_fresh_stream_retry() {
        for exactly_once in [false, true] {
            for (code, want_attempts) in [
                (Code::PermissionDenied, 2),
                (Code::InvalidArgument, 2),
                (Code::Unauthenticated, 2),
                (Code::FailedPrecondition, 2),
                (Code::Unimplemented, 2),
                (Code::NotFound, 2),
                (Code::Unavailable, 4),
            ] {
                let server = FakeWriteServer::new();
                let delivered = scripted(&server, move |_, _| Err(tonic_status(code, "scripted")));
                let logger = Arc::new(RecordingLogger::default());
                let writer = classification_writer(&server, exactly_once, &logger).await;
                let err = writer
                    .append(&Context::background(), &record(&["X"]), None, &mut false)
                    .await
                    .unwrap_err();
                let name = format!("{code}/exactly_once={exactly_once}");
                if exactly_once && code == Code::NotFound {
                    assert_eq!(
                        err.to_string(),
                        "committed write stream already rotated (NotFound)",
                        "{name}"
                    );
                } else {
                    assert_eq!(status_code(&err), code, "{name}: {err}");
                }
                assert_eq!(
                    delivered.lock().unwrap().len(),
                    want_attempts,
                    "{name}: {err}"
                );
                writer.cancel_finalizations();
                writer.wait_finalizations().await;
            }
        }
    }

    // Go TestStreamWriter_TransientInBandErrorIsRetried.
    #[tokio::test(start_paused = true)]
    async fn transient_in_band_errors_are_resent_with_the_same_offset() {
        for exactly_once in [false, true] {
            for code in [
                Code::Unavailable,
                Code::ResourceExhausted,
                Code::Internal,
                Code::DeadlineExceeded,
                Code::Aborted,
            ] {
                let server = FakeWriteServer::new();
                let delivered = scripted(&server, move |attempt, _| {
                    Ok(if attempt <= 2 {
                        in_band(code, "scripted")
                    } else {
                        AppendRowsResponse::default()
                    })
                });
                let logger = Arc::new(RecordingLogger::default());
                let writer = classification_writer(&server, exactly_once, &logger).await;
                let name = format!("{code}/exactly_once={exactly_once}");
                writer
                    .append(
                        &Context::background(),
                        &record(&["X", "Y"]),
                        None,
                        &mut false,
                    )
                    .await
                    .unwrap_or_else(|e| panic!("{name}: {e}"));
                let offsets: Vec<i64> = delivered.lock().unwrap().iter().map(|d| d.1).collect();
                let want = if exactly_once { 0 } else { -1 };
                assert_eq!(offsets, vec![want; 3], "{name}");
                if exactly_once {
                    assert_eq!(writer.stream_offset.load(Ordering::SeqCst), 2, "{name}");
                }
                // One stream carried all three attempts.
                assert_eq!(server.streams_opened(), 1, "{name}");
            }
        }
    }

    // Go TestStreamWriter_InBandErrorRetriesAreBounded.
    #[tokio::test(start_paused = true)]
    async fn in_band_error_retries_are_bounded() {
        for (code, want_attempts) in [
            (Code::Unavailable, 4),
            (Code::ResourceExhausted, 4),
            (Code::PermissionDenied, 1),
            (Code::InvalidArgument, 1),
        ] {
            let server = FakeWriteServer::new();
            let delivered = scripted(&server, move |_, _| Ok(in_band(code, "scripted message")));
            let logger = Arc::new(RecordingLogger::default());
            let writer = classification_writer(&server, false, &logger).await;
            let res = writer
                .append(&Context::background(), &record(&["X"]), None, &mut false)
                .await
                .unwrap();
            // The last response is returned as it is, with BigQuery's message.
            let status = in_band_error(&res).unwrap();
            assert_eq!(status.message, "scripted message", "{code}");
            assert_eq!(delivered.lock().unwrap().len(), want_attempts, "{code}");
        }
    }

    // Go TestBatchProcessor_ExactlyOnceDelivery_AmbiguousSendThenAlreadyExistsAdvancesOffset.
    #[tokio::test(start_paused = true)]
    async fn ambiguous_send_then_already_exists_advances_the_offset() {
        for via_response in [true, false] {
            let server = FakeWriteServer::new();
            let delivered = scripted(&server, move |attempt, _| match attempt {
                1 => Err(tonic_status(Code::Unavailable, "lost response")),
                _ if via_response => Ok(in_band(Code::AlreadyExists, "already committed")),
                _ => Err(tonic_status(Code::AlreadyExists, "already committed")),
            });
            let logger = Arc::new(RecordingLogger::default());
            let writer = classification_writer(&server, true, &logger).await;
            let res = writer
                .append(
                    &Context::background(),
                    &record(&["X", "Y"]),
                    None,
                    &mut false,
                )
                .await
                .unwrap();
            assert_eq!(res, AppendRowsResponse::default());
            assert_eq!(writer.stream_offset.load(Ordering::SeqCst), 2);
            let delivered = delivered.lock().unwrap().clone();
            assert_eq!(delivered, vec![(committed(1), 0), (committed(1), 0)]);
            let state = writer.state.lock().await;
            assert!(!state.offset_desynced && !state.freshly_rotated);
        }
    }

    // Go TestBatchProcessor_ExactlyOnceDelivery_NotFoundAfterLostResponseIsNotResent, then
    // the next batch replaces the stream first (Go's offsetDesynced prologue).
    #[tokio::test(start_paused = true)]
    async fn not_found_after_a_lost_response_is_not_resent() {
        let server = FakeWriteServer::new();
        let delivered = scripted(&server, |attempt, _| match attempt {
            1 => Err(tonic_status(Code::Unavailable, "lost connection")),
            2 => Err(tonic_status(Code::NotFound, "stream expired or missing")),
            _ => Ok(AppendRowsResponse::default()),
        });
        let logger = Arc::new(RecordingLogger::default());
        let writer = classification_writer(&server, true, &logger).await;
        let err = writer
            .append(
                &Context::background(),
                &record(&["A", "B"]),
                None,
                &mut false,
            )
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "batch outcome unknown on stream (NotFound); dropped to prevent duplicate delivery"
        );
        assert_eq!(
            delivered.lock().unwrap().clone(),
            vec![(committed(1), 0), (committed(1), 0)]
        );
        assert!(writer.state.lock().await.offset_desynced);
        assert!(!writer.is_rotation_in_cooldown().await);

        let mut rotated = false;
        writer
            .append(&Context::background(), &record(&["C"]), None, &mut rotated)
            .await
            .unwrap();
        assert!(rotated);
        assert_eq!(delivered.lock().unwrap()[2], (committed(2), 0));
        assert_eq!(writer.stream_offset.load(Ordering::SeqCst), 1);
        writer.wait_finalizations().await;
        let finalized: Vec<String> = server
            .finalize_calls()
            .into_iter()
            .map(|c| c.request.name)
            .collect();
        assert_eq!(finalized, vec![committed(1)]);
    }

    // No Go counterpart (Go's append cannot be abandoned): an `append` future dropped while
    // its response is pending leaves a stream holding that unread response; the next `append`
    // opens a fresh stream instead of reading it, and with exactly-once replaces the committed
    // stream (the dropped batch's outcome is unknown).
    #[tokio::test(start_paused = true)]
    async fn a_dropped_append_resets_its_stream_before_reuse() {
        for exactly_once in [false, true] {
            let server = FakeWriteServer::new();
            let delivered: Delivered = Arc::default();
            let log = Arc::clone(&delivered);
            server.on_append_rows(Arc::new(move |mut stream: AppendRowsServerStream| {
                let log = Arc::clone(&log);
                Box::pin(async move {
                    while let Some(req) = stream.recv().await? {
                        let first = {
                            let mut log = log.lock().unwrap();
                            log.push((
                                req.write_stream.clone(),
                                req.offset.map_or(-1, |o| o.value),
                            ));
                            log.len() == 1
                        };
                        if first {
                            // Never answer the first request.
                            stream.cancelled().await;
                            return Ok(());
                        }
                        stream.send(AppendRowsResponse::default()).await?;
                    }
                    Ok(())
                })
            }));
            let logger = Arc::new(RecordingLogger::default());
            let writer = classification_writer(&server, exactly_once, &logger).await;

            let dropped = tokio::time::timeout(
                Duration::from_secs(1),
                writer.append(
                    &Context::background(),
                    &record(&["A", "B"]),
                    None,
                    &mut false,
                ),
            )
            .await;
            assert!(dropped.is_err(), "the first append must still be waiting");
            assert!(writer.state.lock().await.request_in_flight);

            let mut rotated = false;
            let res = tokio::time::timeout(
                Duration::from_secs(10),
                writer.append(&Context::background(), &record(&["C"]), None, &mut rotated),
            )
            .await
            .expect("the next append must not wait on the dropped request's stream")
            .unwrap();
            assert_eq!(res, AppendRowsResponse::default());
            assert_eq!(server.streams_opened(), 2, "exactly_once={exactly_once}");
            assert!(!writer.state.lock().await.request_in_flight);
            let delivered = delivered.lock().unwrap().clone();
            if exactly_once {
                assert!(rotated);
                assert_eq!(delivered, vec![(committed(1), 0), (committed(2), 0)]);
                assert_eq!(writer.stream_offset.load(Ordering::SeqCst), 1);
                assert!(!writer.state.lock().await.offset_desynced);
                writer.wait_finalizations().await;
                let finalized: Vec<String> = server
                    .finalize_calls()
                    .into_iter()
                    .map(|c| c.request.name)
                    .collect();
                assert_eq!(finalized, vec![committed(1)]);
            } else {
                assert!(!rotated);
                let default_stream = format!("{PARENT}/_default");
                assert_eq!(
                    delivered,
                    vec![(default_stream.clone(), -1), (default_stream, -1)]
                );
            }
        }
    }

    // Go TestBatchProcessor_ExactlyOnceDelivery_RotationCooldownAndGuards (the cooldown).
    #[tokio::test(start_paused = true)]
    async fn failed_rotation_arms_the_cooldown() {
        let server = FakeWriteServer::new();
        let fail_create = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let fail = Arc::clone(&fail_create);
        server.on_create_write_stream(Arc::new(move |call| {
            let fail = fail.load(Ordering::SeqCst);
            Box::pin(async move {
                if fail {
                    return Err(tonic_status(Code::PermissionDenied, "denied"));
                }
                Ok(WriteStream {
                    name: format!("{}/streams/replacement", call.request.parent),
                    ..WriteStream::default()
                })
            })
        }));
        let delivered = scripted(&server, |_, _| Ok(AppendRowsResponse::default()));
        let logger = Arc::new(RecordingLogger::default());
        let writer = writer_on(
            &Context::background(),
            Some(server.client().unwrap()),
            &committed(1),
            test_config(true, &logger),
        );
        writer.state.lock().await.offset_desynced = true;

        let err = writer
            .append(&Context::background(), &record(&["X"]), None, &mut false)
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "committed stream out of sync and not replaced: failed to create committed write \
             stream: rpc error: code = PermissionDenied desc = denied"
        );
        assert!(writer.is_rotation_in_cooldown().await);

        // Within the cooldown: no CreateWriteStream, no request.
        tokio::time::advance(Duration::from_secs(29)).await;
        let err = writer
            .append(&Context::background(), &record(&["X"]), None, &mut false)
            .await
            .unwrap_err();
        assert!(
            err.to_string()
                .starts_with("committed write stream rotation in cooldown until "),
            "{err}"
        );
        assert_eq!(server.create_calls().len(), 1);

        // A batch that already rotated does not rotate again.
        tokio::time::advance(Duration::from_secs(2)).await;
        let err = writer
            .append(&Context::background(), &record(&["X"]), None, &mut true)
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "committed write stream already rotated in batch"
        );

        // After the cooldown the next batch replaces the stream and is written.
        fail_create.store(false, Ordering::SeqCst);
        writer
            .append(&Context::background(), &record(&["X"]), None, &mut false)
            .await
            .unwrap();
        assert_eq!(server.create_calls().len(), 2);
        assert_eq!(
            delivered.lock().unwrap().clone(),
            vec![(format!("{PARENT}/streams/replacement"), 0)]
        );
        assert!(!writer.is_rotation_in_cooldown().await);
        writer.wait_finalizations().await;
    }

    // Go TestBatchProcessor_ExactlyOnceDelivery_HangingCreateWriteStreamArmsCooldown and the
    // starved exception of rotateCommittedStream.
    #[tokio::test(start_paused = true)]
    async fn hanging_create_arms_the_cooldown_unless_the_call_was_starved() {
        for (caller_timeout, want_cooldown) in [
            (Duration::from_secs(20), true),
            (Duration::from_secs(1), false),
        ] {
            let server = FakeWriteServer::new();
            server.on_create_write_stream(Arc::new(|_| Box::pin(std::future::pending())));
            let logger = Arc::new(RecordingLogger::default());
            let writer = writer_on(
                &Context::background(),
                Some(server.client().unwrap()),
                &committed(1),
                test_config(true, &logger),
            );
            let ctx = Context::background().with_timeout(caller_timeout);
            let mut state = writer.state.lock().await;
            let err = writer
                .rotate_committed_stream(&mut state, &ctx)
                .await
                .unwrap_err();
            assert!(
                err.to_string()
                    .starts_with("failed to create committed write stream: "),
                "{err}"
            );
            assert_eq!(
                state.rotate_retry_at.is_some(),
                want_cooldown,
                "{caller_timeout:?}"
            );
            assert_eq!(state.stream_name, committed(1));
        }
    }

    /// Go's `scriptedCommittedStreamServer` with OutOfRange on the first attempt (the first
    /// stream is retired) and `on_finalize` for FinalizeWriteStream.
    fn retiring_server(
        on_finalize: impl Fn() -> gadk_platform::BoxFuture<
            'static,
            Result<FinalizeWriteStreamResponse, tonic::Status>,
        > + Send
        + Sync
        + 'static,
    ) -> FakeWriteServer {
        let server = FakeWriteServer::new();
        scripted(&server, |attempt, _| {
            if attempt == 1 {
                Err(tonic_status(Code::OutOfRange, "offset beyond end"))
            } else {
                Ok(AppendRowsResponse::default())
            }
        });
        server.on_finalize_write_stream(Arc::new(move |_| on_finalize()));
        server
    }

    // Go TestBatchProcessor_ExactlyOnceDelivery_RetiredStreamFinalizeKeepsMetadataAndLogsErrors
    // (and ..._FinalizePreservesCallerContextMetadata).
    #[tokio::test(start_paused = true)]
    async fn retired_stream_finalize_keeps_metadata_and_logs_errors() {
        let server = retiring_server(|| {
            Box::pin(async {
                tokio::time::sleep(Duration::from_millis(35)).await;
                Err(tonic_status(Code::PermissionDenied, "finalize denied"))
            })
        });
        let caller = Context::background()
            .append_outgoing_metadata(&[("x-test-caller", "preserve-me")])
            .with_cancel();
        let logger = Arc::new(RecordingLogger::default());
        let writer = writer_on(
            &caller,
            Some(server.client().unwrap()),
            &committed(1),
            test_config(true, &logger),
        );
        // The writer ignores the cancellation of its constructor's context but keeps its
        // values.
        caller.cancel();
        let mut rotated = false;
        writer
            .append(
                &Context::background(),
                &record(&["LLM_REQUEST"]),
                None,
                &mut rotated,
            )
            .await
            .unwrap();
        assert!(rotated);
        writer.wait_finalizations().await;
        assert_eq!(
            logger.lines(),
            vec![format!(
                "agentanalytics: failed to finalize write stream {}: rpc error: code = \
                 PermissionDenied desc = finalize denied",
                committed(1)
            )]
        );
        let calls = server.finalize_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].request.name, committed(1));
        assert_eq!(
            calls[0].metadata.get("x-test-caller"),
            Some(&vec!["preserve-me".to_owned()])
        );
        // Bounded by the rotate timeout.
        let timeout = calls[0].timeout.unwrap();
        assert!(timeout <= DEFAULT_ROTATE_TIMEOUT, "{timeout:?}");
        // Both AppendRows streams carried the constructor's values too.
        for md in server.append_stream_metadata() {
            assert_eq!(
                md.get("x-test-caller"),
                Some(&vec!["preserve-me".to_owned()])
            );
        }
    }

    // Go TestBatchProcessor_ExactlyOnceDelivery_RetiredStreamFinalizeBoundedByRotateTimeout.
    #[tokio::test(start_paused = true)]
    async fn retired_stream_finalize_is_bounded_by_the_rotate_timeout() {
        let server = retiring_server(|| Box::pin(std::future::pending()));
        let logger = Arc::new(RecordingLogger::default());
        let writer = writer_on(
            &Context::background(),
            Some(server.client().unwrap()),
            &committed(1),
            test_config(true, &logger),
        );
        writer.state.lock().await.rotate_timeout = Duration::from_millis(50);
        writer
            .append(
                &Context::background(),
                &record(&["LLM_REQUEST"]),
                None,
                &mut false,
            )
            .await
            .unwrap();
        let start = Instant::now();
        writer.wait_finalizations().await;
        assert!(start.elapsed() <= Duration::from_millis(50));
        assert!(
            logger.contains(&format!("failed to finalize write stream {}", committed(1))),
            "{logger}"
        );
        assert!(logger.contains("context deadline exceeded"), "{logger}");
    }

    // Go cancelFinalizations / waitFinalizations as Close uses them.
    #[tokio::test(start_paused = true)]
    async fn cancel_finalizations_ends_a_hanging_finalization() {
        let server = retiring_server(|| Box::pin(std::future::pending()));
        let logger = Arc::new(RecordingLogger::default());
        let writer = writer_on(
            &Context::background(),
            Some(server.client().unwrap()),
            &committed(1),
            test_config(true, &logger),
        );
        writer
            .append(
                &Context::background(),
                &record(&["LLM_REQUEST"]),
                None,
                &mut false,
            )
            .await
            .unwrap();
        server.wait_for_append_requests(2).await;
        let start = Instant::now();
        writer.cancel_finalizations();
        writer.wait_finalizations().await;
        assert!(start.elapsed() < DEFAULT_ROTATE_TIMEOUT);
        assert!(logger.contains("context canceled"), "{logger}");
    }

    fn fixture(rel: &str) -> Value {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(rel);
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    /// grpc-go's and tonic's own transport headers, left out of the comparison.
    const TRANSPORT_KEYS: [&str; 5] = [
        "content-type",
        "grpc-accept-encoding",
        "user-agent",
        "te",
        "grpc-encoding",
    ];

    // Spec R5: the AppendRows routing header for named, default, escaped and empty stream names
    // (gapic/metadata.json's streamWriter.append cases).
    #[tokio::test(start_paused = true)]
    async fn append_rows_metadata_matches_the_probe() {
        let cases = fixture("gapic/metadata.json");
        let mut seen = 0;
        for case in cases["cases"].as_array().unwrap() {
            let name = case["name"].as_str().unwrap();
            if !name.starts_with("append_rows_") {
                continue;
            }
            seen += 1;
            let call = &case["calls"][0];
            let want_req = &call["requests"][0];
            let stream_name = want_req["write_stream"].as_str().unwrap();
            let server = FakeWriteServer::new();
            let ctx = if name == "append_rows_named_stream" {
                Context::background().append_outgoing_metadata(&[("x-test-caller", "preserve-me")])
            } else {
                Context::background()
            };
            let logger = Arc::new(RecordingLogger::default());
            let writer = writer_on(
                &ctx,
                Some(server.client().unwrap()),
                stream_name,
                test_config(name == "append_rows_exactly_once", &logger),
            );
            writer
                .append(&Context::background(), &record(&["X"]), None, &mut false)
                .await
                .unwrap();

            let mut want: Metadata = serde_json::from_value(call["metadata"].clone()).unwrap();
            want.retain(|k, _| !TRANSPORT_KEYS.contains(&k.as_str()));
            want.insert(
                "x-goog-api-client".to_owned(),
                vec![gadk_platform::tracking_header()],
            );
            let mut got = server.append_stream_metadata()[0].clone();
            got.retain(|k, _| !TRANSPORT_KEYS.contains(&k.as_str()));
            assert_eq!(got, want, "{name}");

            let reqs = server.append_requests();
            assert_eq!(reqs.len(), 1, "{name}");
            let req = &reqs[0].request;
            assert_eq!(req.write_stream, stream_name, "{name}");
            assert_eq!(req.trace_id, stream_writer_trace_id(), "{name}");
            assert_eq!(
                req.offset.map(|o| o.value),
                want_req["offset"].as_i64(),
                "{name}"
            );
            #[allow(deprecated)]
            let row_count = match &req.rows {
                Some(Rows::ArrowRows(data)) => data.rows.as_ref().map(|r| r.row_count),
                _ => None,
            };
            assert_eq!(row_count, want_req["row_count"].as_i64(), "{name}");
        }
        assert_eq!(seen, 5);
    }

    // Spec R5: newAppendRowsRequestLocked / serializedRequestSize on the append_rows/ cases
    // (Go's writer schema and batch bytes; gadk's trace id in the place of the probe's).
    #[tokio::test]
    async fn serialized_request_size_matches_the_probe() {
        let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let cases = fixture("append_rows/requests.json");
        for case in cases["cases"].as_array().unwrap() {
            let name = case["name"].as_str().unwrap();
            let schema =
                std::fs::read(fixtures.join(case["writer_schema_file"].as_str().unwrap())).unwrap();
            let batch = case["batch_file"]
                .as_str()
                .map(|f| std::fs::read(fixtures.join(f)).unwrap())
                .unwrap_or_default();
            let num_rows = case["num_rows"].as_i64().unwrap();
            let logger = Arc::new(RecordingLogger::default());
            let writer = StreamWriter::new(
                &Context::background(),
                None,
                case["stream_name"].as_str().unwrap(),
                schema.clone(),
                test_config(case["exactly_once"].as_bool().unwrap(), &logger),
            );
            writer
                .stream_offset
                .store(case["stream_offset"].as_i64().unwrap(), Ordering::SeqCst);

            let go_trace = case["trace_id"].as_str().unwrap();
            let byte_len = case["byte_len"].as_u64().unwrap() as usize;
            assert_eq!(
                writer.serialized_request_size(&batch, num_rows).await,
                byte_len - go_trace.len() + stream_writer_trace_id().len(),
                "{name}"
            );
            let state = writer.state.lock().await;
            assert_eq!(
                writer.new_append_rows_request_locked(&state, &batch, num_rows),
                append_rows_request(
                    case["stream_name"].as_str().unwrap(),
                    stream_writer_trace_id(),
                    &schema,
                    &batch,
                    num_rows,
                    case["offset"].as_i64(),
                ),
                "{name}"
            );
        }
    }

    // Go TestStreamWriterAppend_UsesPreSerializedBatchBytes.
    #[tokio::test]
    async fn append_uses_pre_serialized_batch_bytes() {
        let logger = Arc::new(RecordingLogger::default());
        let writer = writer_on(
            &Context::background(),
            None,
            "s",
            test_config(false, &logger),
        );
        let counts = Arc::new(MockCounts::default());
        writer.state.lock().await.stream = Some(Box::new(MockStream {
            recv_res: Some(AppendRowsResponse::default()),
            counts: Arc::clone(&counts),
            ..MockStream::default()
        }));
        let pre = serialize_record(&record(&["pre-serialized"]), None).unwrap();
        let fallback = record(&["fallback-record", "second"]);
        writer
            .append(
                &Context::background(),
                &fallback,
                Some(pre.clone()),
                &mut false,
            )
            .await
            .unwrap();
        writer
            .append(&Context::background(), &fallback, None, &mut false)
            .await
            .unwrap();
        let sent = counts.sent.lock().unwrap().clone();
        let batch_of = |req: &AppendRowsRequest| match &req.rows {
            Some(Rows::ArrowRows(data)) => data.rows.clone().unwrap().serialized_record_batch,
            _ => panic!("not arrow"),
        };
        assert_eq!(batch_of(&sent[0]), pre);
        assert_eq!(
            batch_of(&sent[1]),
            serialize_record(&fallback, None).unwrap()
        );
        assert!(logger.lines().is_empty());
    }

    #[tokio::test]
    async fn an_ended_context_fails_before_sending() {
        let logger = Arc::new(RecordingLogger::default());
        let writer = writer_on(
            &Context::background(),
            None,
            "s",
            test_config(false, &logger),
        );
        let counts = Arc::new(MockCounts::default());
        writer.state.lock().await.stream = Some(Box::new(MockStream {
            counts: Arc::clone(&counts),
            ..MockStream::default()
        }));
        let ctx = Context::background().with_cancel();
        ctx.cancel();
        let err = writer
            .append(&ctx, &record(&["X"]), None, &mut false)
            .await
            .unwrap_err();
        assert!(err.is_context(ContextError::Canceled), "{err}");
        assert_eq!(counts.sends.load(Ordering::SeqCst), 0);
    }

    // Go's AfterFunc(ctx, streamCancel): the caller's deadline ends a stalled stream and the
    // batch with the context's error.
    #[tokio::test(start_paused = true)]
    async fn the_callers_context_ends_a_stalled_stream() {
        let server = FakeWriteServer::new();
        server.on_append_rows(Arc::new(|mut stream: AppendRowsServerStream| {
            Box::pin(async move {
                let _ = stream.recv().await;
                stream.cancelled().await;
                Ok(())
            })
        }));
        let logger = Arc::new(RecordingLogger::default());
        let writer = writer_on(
            &Context::background(),
            Some(server.client().unwrap()),
            "s",
            test_config(false, &logger),
        );
        let ctx = Context::background().with_timeout(Duration::from_secs(1));
        let err = writer
            .append(&ctx, &record(&["X"]), None, &mut false)
            .await
            .unwrap_err();
        assert!(err.is_context(ContextError::DeadlineExceeded), "{err}");
        let state = writer.state.lock().await;
        assert!(state.stream.is_none() && state.stream_ctx.is_none());
    }

    #[tokio::test]
    async fn without_a_client_streams_cannot_open() {
        let logger = Arc::new(RecordingLogger::default());
        let mut config = test_config(false, &logger);
        config.retry_config.max_retries = 0;
        let writer = writer_on(&Context::background(), None, "s", config.clone());
        let err = writer
            .append(&Context::background(), &record(&["X"]), None, &mut false)
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "max retries exceeded: failed to open append rows stream: no write client"
        );
        let err = create_committed_write_stream(&Context::background(), None, &config)
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "no write client");
        // Rotation fails the same way (and arms the cooldown).
        let mut state = writer.state.lock().await;
        let err = writer
            .rotate_committed_stream(&mut state, &Context::background())
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "no write client");
        assert!(state.rotate_retry_at.is_some());
    }

    #[tokio::test(start_paused = true)]
    async fn create_committed_write_stream_follows_go() {
        let server = FakeWriteServer::new();
        let client = server.client().unwrap();
        let logger = Arc::new(RecordingLogger::default());
        let config = test_config(true, &logger);
        let name = create_committed_write_stream(&Context::background(), Some(&client), &config)
            .await
            .unwrap();
        assert_eq!(name, committed(1));
        let calls = server.create_calls();
        assert_eq!(calls[0].request.parent, PARENT);
        assert_eq!(
            calls[0].request.write_stream.as_ref().unwrap().r#type,
            write_stream::Type::Committed as i32
        );
        server.on_create_write_stream(Arc::new(|_| {
            Box::pin(async { Err(tonic_status(Code::PermissionDenied, "no")) })
        }));
        let err = create_committed_write_stream(&Context::background(), Some(&client), &config)
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "failed to create committed write stream: rpc error: code = PermissionDenied desc = no"
        );
        assert_eq!(status_code(&err), Code::PermissionDenied);
    }

    #[tokio::test]
    async fn streams_to_finalize_and_close_active_stream() {
        let logger = Arc::new(RecordingLogger::default());
        let writer = writer_on(
            &Context::background(),
            None,
            "s1",
            test_config(true, &logger),
        );
        let stream_ctx = Context::background().with_cancel();
        {
            let mut state = writer.state.lock().await;
            state.stream = Some(Box::new(MockStream::default()));
            state.stream_ctx = Some(stream_ctx.clone());
        }
        writer.close_active_stream().await;
        assert!(stream_ctx.err().is_some());
        {
            let state = writer.state.lock().await;
            assert!(state.stream.is_none() && state.stream_ctx.is_none());
        }
        assert_eq!(writer.streams_to_finalize().await, vec!["s1".to_owned()]);
        assert!(writer.streams_to_finalize().await.is_empty());
        writer.wait_finalizations().await;
    }
}
