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

//! Port of ADK-Go `plugin/agentanalytics/batch_processor.go`'s `streamWriter`: the persistent
//! AppendRows stream with per-request send/receive, Go's error classes
//! (`isPermanentWriteError`, `isTransientInBandError`, `isAmbiguousSendError`), committed-stream
//! rotation (`rotateCommittedStream`) with its cooldown and background finalization of retired
//! streams, exactly-once offsets (`handleExactlyOnceError`), `newAppendRowsRequestLocked` /
//! `serializedRequestSize`, `createCommittedWriteStream` and `streamWriterTraceID[ForVersion]`.

#![allow(dead_code, unused_variables)] // Task 8 implements the bodies.

use std::sync::Arc;
use std::sync::atomic::AtomicI64;
use std::time::Duration;

use arrow_array::RecordBatch;
use tokio::time::Instant;
use tokio_util::task::TaskTracker;

use crate::config::Config;
use crate::context::Context;
use crate::error::{Code, Error};
use crate::storagepb::{AppendRowsRequest, AppendRowsResponse};
use crate::write::AppendRowsClient;
use crate::write::gapic::WriteClient;

/// Go `streamWriterTraceIDPrefix`, gadk's own (Go: `google-adk-bq-logger-go`).
// Divergence (ledger 7b-2): the AppendRows trace id names gadk and the gadk-agentanalytics
// crate version instead of ADK-Go's prefix and module version.
pub const STREAM_WRITER_TRACE_ID_PREFIX: &str = "gadk-bq-logger-rust";

/// Go `defaultRotateTimeout`: bounds `CreateWriteStream` of a rotation and the finalization of
/// a retired stream.
pub(crate) const DEFAULT_ROTATE_TIMEOUT: Duration = Duration::from_secs(30);

/// Go `streamWriterTraceID()`: `gadk-bq-logger-rust/<CARGO_PKG_VERSION>`.
pub fn stream_writer_trace_id() -> &'static str {
    todo!("Task 8")
}

/// Go `streamWriterTraceIDForVersion(v)`: the prefix, and `/<v>` unless `v` is empty.
pub fn stream_writer_trace_id_for_version(version: &str) -> String {
    todo!("Task 8")
}

/// Go `newAppendRowsRequestLocked`'s request: `write_stream`, `trace_id` and
/// `arrow_rows.writer_schema` always, `arrow_rows.rows` (`serialized_record_batch`,
/// `row_count`), `offset` only when `Some` (exactly-once; offset 0 is a present, empty
/// `Int64Value`).
pub fn append_rows_request(
    write_stream: &str,
    trace_id: &str,
    schema_bytes: &[u8],
    batch_bytes: &[u8],
    num_rows: i64,
    offset: Option<i64>,
) -> AppendRowsRequest {
    todo!("Task 8")
}

/// Go `createCommittedWriteStream(ctx, writeClient, config)`: a `COMMITTED` stream on
/// `projects/<p>/datasets/<d>/tables/<t>`, returning its name.
///
/// # Errors
///
/// `writeClient is nil` without a client; `failed to create committed write stream: <err>`.
pub(crate) async fn create_committed_write_stream(
    ctx: &Context,
    client: Option<&WriteClient>,
    config: &Config,
) -> Result<String, Error> {
    todo!("Task 8")
}

/// Go `isTransientInBandError(code)`: Unavailable, ResourceExhausted, Internal,
/// DeadlineExceeded, Aborted.
pub(crate) fn is_transient_in_band_error(code: Code) -> bool {
    todo!("Task 8")
}

/// Go `isAmbiguousSendError(code)`: Unavailable, DeadlineExceeded, Canceled, Aborted, Internal,
/// Unknown.
pub(crate) fn is_ambiguous_send_error(code: Code) -> bool {
    todo!("Task 8")
}

/// The fields of Go's `streamWriter` that its `mu` guards (and the test hooks Go tests set on
/// it directly).
pub(crate) struct StreamState {
    /// Go `streamName`.
    pub(crate) stream_name: String,
    /// Go `stream`: the open AppendRows stream, if any.
    pub(crate) stream: Option<Box<dyn AppendRowsClient>>,
    /// Go `streamCtx` / `streamCancel`: the open stream's context (`cancel()` is Go's
    /// `streamCancel`).
    pub(crate) stream_ctx: Option<Context>,
    /// Go `offsetDesynced`.
    pub(crate) offset_desynced: bool,
    /// Go `freshlyRotated`.
    pub(crate) freshly_rotated: bool,
    /// Go `rotateRetryAt`; `None` is Go's zero time (no cooldown).
    pub(crate) rotate_retry_at: Option<Instant>,
    /// Go `rotateTimeout` (tests shorten it); zero means [`DEFAULT_ROTATE_TIMEOUT`].
    pub(crate) rotate_timeout: Duration,
    /// Go `now` (tests pin the clock); `None` reads [`Instant::now`].
    pub(crate) now: Option<Arc<dyn Fn() -> Instant + Send + Sync>>,
}

/// Go `streamWriter`: a persistent AppendRows stream on one write stream.
pub(crate) struct StreamWriter {
    /// Go `writeClient`; `None` is Go's nil client (rotation then fails with `writeClient is
    /// nil`, opening a stream is not possible).
    pub(crate) write_client: Option<WriteClient>,
    /// Go `schemaBytes`: the serialized Arrow writer schema of every request.
    pub(crate) schema_bytes: Vec<u8>,
    /// Go `config`.
    pub(crate) config: Config,
    /// Go `baseCtx`: `without_cancel` of the constructor's context (keeps its outgoing
    /// metadata).
    pub(crate) base_ctx: Context,
    /// Go `finalizeCtx` (`cancel()` is Go's `finalizeCancel`): parent of retired-stream
    /// finalizations.
    pub(crate) finalize_ctx: Context,
    /// Go `finalizeWg`: the running finalizations of retired streams.
    pub(crate) finalizations: TaskTracker,
    /// Go `streamOffset`.
    pub(crate) stream_offset: AtomicI64,
    /// Go `mu` and the fields it guards.
    pub(crate) state: tokio::sync::Mutex<StreamState>,
}

impl StreamWriter {
    /// Go `newStreamWriter(ctx, client, streamName, schemaBytes, config)`.
    pub(crate) fn new(
        ctx: &Context,
        client: Option<WriteClient>,
        stream_name: &str,
        schema_bytes: Vec<u8>,
        config: Config,
    ) -> StreamWriter {
        todo!("Task 8")
    }

    /// Go `append(ctx, recordBatch, batchBytes, rotatedInBatch)`: sends `record` (as
    /// `batch_bytes` when given, else serialized once and reused) and returns BigQuery's
    /// response, retrying per `config.retry_config`; `rotated_in_batch` is the rotation budget
    /// the chunks of one batch share.
    ///
    /// # Errors
    ///
    /// Go's texts: `max retries exceeded: …`, `failed to open append rows stream: …`, `failed to
    /// send row batch: …`, `failed to receive response for row batch: …`, the exactly-once
    /// rotation errors, the context's error.
    pub(crate) async fn append(
        &self,
        ctx: &Context,
        record: &RecordBatch,
        batch_bytes: Option<Vec<u8>>,
        rotated_in_batch: &mut bool,
    ) -> Result<AppendRowsResponse, Error> {
        todo!("Task 8")
    }

    /// Go `newAppendRowsRequestLocked(batchBytes, numRows)` ([`append_rows_request`] with this
    /// writer's stream, trace id, schema and, with exactly-once, `stream_offset`).
    pub(crate) fn new_append_rows_request_locked(
        &self,
        state: &StreamState,
        batch_bytes: &[u8],
        num_rows: i64,
    ) -> AppendRowsRequest {
        todo!("Task 8")
    }

    /// Go `serializedRequestSize(batchBytes, numRows)`: the encoded length of the request
    /// `append` would send.
    pub(crate) async fn serialized_request_size(&self, batch_bytes: &[u8], num_rows: i64) -> usize {
        todo!("Task 8")
    }

    /// Go `isPermanentWriteError(code)`: PermissionDenied, InvalidArgument, Unauthenticated,
    /// FailedPrecondition, Unimplemented; NotFound unless exactly-once.
    pub(crate) fn is_permanent_write_error(&self, code: Code) -> bool {
        todo!("Task 8")
    }

    /// Go `isRotationInCooldown()`.
    pub(crate) async fn is_rotation_in_cooldown(&self) -> bool {
        todo!("Task 8")
    }

    /// Go `closeActiveStream()`.
    pub(crate) async fn close_active_stream(&self) {
        todo!("Task 8")
    }

    /// Go `cancelFinalizations()`.
    pub(crate) fn cancel_finalizations(&self) {
        todo!("Task 8")
    }

    /// Go `waitFinalizations()`.
    pub(crate) async fn wait_finalizations(&self) {
        todo!("Task 8")
    }

    /// Go `streamsToFinalize()`: the current stream's name (taken), or none.
    pub(crate) async fn streams_to_finalize(&self) -> Vec<String> {
        todo!("Task 8")
    }

    /// Go `nowOrDefault()`.
    pub(crate) fn now_or_default(state: &StreamState) -> Instant {
        todo!("Task 8")
    }

    /// Go `rotateTimeoutOrDefault()`.
    pub(crate) fn rotate_timeout_or_default(state: &StreamState) -> Duration {
        todo!("Task 8")
    }

    /// Go `resetStream()`.
    pub(crate) fn reset_stream(state: &mut StreamState) {
        todo!("Task 8")
    }

    /// Go `rotateCommittedStream(ctx)` (caller holds `state`).
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
        todo!("Task 8")
    }

    /// Go `handleExactlyOnceError(ctx, code, numRows, &hadAmbiguousSend, &rotatedInBatch,
    /// sendSucceeded)` (caller holds `state`): `Ok(Some(response))` is Go's `done` (the batch
    /// is confirmed), `Ok(None)` retries, `Err` is Go's `rotateErr`.
    ///
    /// # Errors
    ///
    /// Go's `batch outcome unknown on stream (…)…`, `committed write stream already rotated
    /// (…)`, `failed to rotate committed write stream after …: …`.
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
        todo!("Task 8")
    }
}
