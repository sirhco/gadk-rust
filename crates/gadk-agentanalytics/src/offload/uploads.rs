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

//! Port of the upload orchestration of ADK-Go `plugin/agentanalytics/gcs_offloader.go`
//! (`impl BatchProcessor`): `resolveUploadsForBatch` with its concurrency limit, batch timeout
//! (told apart from the caller's deadline by the context's cause), cooldown and deferred
//! uploads (`clearDeferredUploads`, `dropDeferredUploadsQuietly`, `requeueDeferredUploads`,
//! `logDeferredUploadFailures`), `objectPath`, `upload` (offloader panics caught),
//! `applyUpload`, `logUploadFailure`, and the pure helpers (`gcsMetadataDetails`,
//! `textPreview`, `extensionForMIME`, `isSafeExtension`, `pathSegment`, `gcsObjectPath`,
//! `safeUploadErrorSummary`, `stripURIQueries`).

use std::any::Any;
use std::future::Future;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::Poll;
use std::time::Duration;

use chrono::{DateTime, NaiveDate, Utc};
use serde_json::json;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tokio::time::Instant;

use super::{
    FALLBACK_EXTENSION, MEDIA_OFFLOADED_TEXT, MIME_EXTENSIONS, MISSING_EVENT_ID, MISSING_SPAN_ID,
    MISSING_TRACE_ID, OffloaderPanicError, OwnedUploadError, TEXT_PREVIEW_RUNES,
    TEXT_PREVIEW_SUFFIX, UploadSentinel,
};
use crate::batch_processor::BatchProcessor;
use crate::config::{
    DEFAULT_GCS_MAX_CONCURRENT_UPLOADS, GCS_UPLOAD_TIMEOUT, GcsContentOffloader, GcsUploadRequest,
};
use crate::context::{Context, ContextError};
use crate::error::Error;
use crate::formatter::{
    INLINE_DATA_TEXT, PendingUpload, STORAGE_MODE_GCS_REFERENCE, UPLOAD_FAILED_TEXT, object_ref,
};
use crate::redaction::{
    COLUMN_CONTENT_PARTS, URI_TOKEN_RE, panic_type_name, redact_error_message,
    trim_trailing_punctuation,
};
use crate::row::{
    DeferredRowUploads, DeferredUpload, QueuedRow, Row, RowExt, RowValue, SharedSummaryBuilder,
    apply_summary_to_row, from_json_map,
};

/// Go `eventIDField`.
const EVENT_ID_FIELD: &str = "event_id";

/// Go `timestampField`.
const TIMESTAMP_FIELD: &str = "timestamp";

/// Go `uploadResolveOptions`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct UploadResolveOptions {
    /// Go `pauseOnCallerDeadline`.
    pub(crate) pause_on_caller_deadline: bool,
    /// Go `canDefer`.
    pub(crate) can_defer: bool,
}

/// Go `uploadTask`: one payload to upload and the `content_parts` entry it rewrites, located
/// by index (the batch row and the entry's slot) rather than by Go's map pointers.
#[derive(Debug)]
pub(crate) struct UploadTask {
    /// The index of the row in the batch (Go `row`).
    pub(crate) row: usize,
    /// The index of the entry in the row's `content_parts` (Go `part`, `pending.slot`).
    pub(crate) slot: usize,
    /// Go `builder`.
    pub(crate) builder: Option<SharedSummaryBuilder>,
    /// Go `pending`.
    pub(crate) pending: PendingUpload,
    /// Go `path`: the object name.
    pub(crate) path: String,
    /// Go `uri`.
    pub(crate) uri: String,
    /// Go `err`.
    pub(crate) err: Option<Error>,
}

/// What Go's `upload` returns: `(uri, started, err)`.
#[derive(Debug)]
pub(crate) struct UploadResult {
    /// Go `uri`.
    pub(crate) uri: String,
    /// Go `started`: the offloader was called.
    pub(crate) started: bool,
    /// Go `err`.
    pub(crate) err: Option<Error>,
}

/// How the uploads of a batch ended (the state Go's `resolveUploadsForBatch` keeps across its
/// `wg.Wait()`).
#[derive(Debug, Clone, Copy)]
struct UploadsOutcome {
    /// Go `ownTimeoutExpired`.
    own_timeout_expired: bool,
    /// Go `paused`.
    paused: bool,
}

/// Which way `resolveUploadsForBatch` went.
enum Stage {
    /// No task or no offloader (Go's first early return).
    NoUploads { offloader_missing: bool },
    /// The uploads are paused (Go's cooldown early return).
    Cooldown,
    /// The uploads ran.
    Uploaded(UploadsOutcome),
}

impl BatchProcessor {
    /// Go `resolveUploadsForBatch(ctx, opts, batch)`: uploads the payloads queued with the rows
    /// of `batch` (at most `gcs_max_concurrent_uploads` at a time, under one batch timeout that
    /// `ctx` can end sooner) and rewrites the matching `content_parts` entries; a failed upload
    /// keeps the part inline. A batch whose uploads show Cloud Storage stalled pauses the
    /// uploads for the length of the timeout. With `opts.can_defer`, when `ctx` (not the batch
    /// timeout) ended the uploads without pausing them, the failed uploads stay reserved on
    /// their rows' [`DeferredRowUploads`] for [`requeue_deferred_uploads`] or
    /// [`clear_deferred_uploads`]; otherwise no row keeps any.
    ///
    /// A [`Logger`](crate::config::Logger) that panics while the results are applied unwinds
    /// through this function after every reservation it took over is released and every
    /// row's deferred uploads are dropped (Go's deferred `completedNormally` check).
    ///
    /// [`requeue_deferred_uploads`]: BatchProcessor::requeue_deferred_uploads
    /// [`clear_deferred_uploads`]: BatchProcessor::clear_deferred_uploads
    pub(crate) async fn resolve_uploads_for_batch(
        &self,
        ctx: &Context,
        opts: UploadResolveOptions,
        batch: &mut [QueuedRow],
    ) {
        // Deferred uploads that a caller left unsettled are dropped before the loop takes over
        // the reservations of the queued payloads, so that a Logger that panics on their
        // failures cannot leak any of those.
        self.clear_deferred_uploads(batch);
        let mut tasks = Vec::new();
        let mut summaries: Vec<(usize, SharedSummaryBuilder)> = Vec::new();
        let mut reserved_total: i64 = 0;
        for (i, q) in batch.iter_mut().enumerate() {
            let builder = q.summary_builder.take();
            if q.raw_parts.is_empty() {
                continue;
            }
            let pending = std::mem::take(&mut q.raw_parts);
            let orig_truncated = q.row.bool("is_truncated").unwrap_or(false);
            let parts_len = q.row.parts(COLUMN_CONTENT_PARTS).map_or(0, <[Row]>::len);
            let mut has_row_tasks = false;
            for u in pending {
                reserved_total += u.reserved;
                if u.slot >= parts_len {
                    continue;
                }
                has_row_tasks = true;
                let path = self.object_path(&q.row, &u);
                tasks.push(UploadTask {
                    row: i,
                    slot: u.slot,
                    builder: builder.clone(),
                    pending: u,
                    path,
                    uri: String::new(),
                    err: None,
                });
            }
            if has_row_tasks {
                q.deferred = Some(DeferredRowUploads {
                    pending: Vec::new(),
                    builder: builder.clone(),
                    non_deferred_truncated: orig_truncated,
                });
                if let Some(builder) = builder {
                    summaries.push((i, builder));
                }
            }
        }

        let stage = match self.config.gcs_offloader.clone() {
            Some(offloader) if !tasks.is_empty() => {
                let upload_timeout = self.batch_upload_timeout();
                let paused_until = *lock(&self.upload_cooldown_until);
                if paused_until.is_some_and(|until| Instant::now() < until) {
                    Stage::Cooldown
                } else {
                    Stage::Uploaded(
                        self.run_uploads(ctx, opts, &offloader, upload_timeout, &mut tasks)
                            .await,
                    )
                }
            }
            offloader => Stage::NoUploads {
                offloader_missing: offloader.is_none(),
            },
        };
        let settled = catch_unwind(AssertUnwindSafe(|| {
            self.settle_uploads(ctx, opts, stage, batch, &mut tasks, &summaries)
        }));
        match settled {
            Ok(deferred_reserved) => self.release_upload_bytes(reserved_total - deferred_reserved),
            Err(panic) => {
                for q in batch.iter_mut() {
                    q.deferred = None;
                }
                self.release_upload_bytes(reserved_total);
                resume_unwind(panic);
            }
        }
    }

    /// Go `cmp.Or(b.uploadTimeout, b.config.GCSUploadTimeout, gcsUploadTimeout)`.
    fn batch_upload_timeout(&self) -> Duration {
        [
            self.upload_timeout_override(),
            self.config.gcs_upload_timeout,
        ]
        .into_iter()
        .find(|d| !d.is_zero())
        .unwrap_or(GCS_UPLOAD_TIMEOUT)
    }

    /// The uploads of `resolveUploadsForBatch`, from `context.WithTimeoutCause` to the pause
    /// decision: writes each task's `uri` and `err`.
    async fn run_uploads(
        &self,
        ctx: &Context,
        opts: UploadResolveOptions,
        offloader: &Arc<dyn GcsContentOffloader>,
        upload_timeout: Duration,
        tasks: &mut [UploadTask],
    ) -> UploadsOutcome {
        // The cause that with_timeout_cause records tells which deadline ended the uploads, so
        // that this does not depend on when the offloaders return.
        let now = Instant::now();
        let timeout_cause = Arc::new(UploadSentinel::BatchUploadTimeout.error());
        let upload_ctx = ctx.with_timeout_cause(upload_timeout, Arc::clone(&timeout_cause));
        // An upload that the deadline ends is a stall if it had been running for at least three
        // quarters of the time that the batch has for its uploads, which ends at the earlier
        // of the batch's own timeout and the deadline of ctx. The time that it waited for a
        // free slot before it started does not count.
        let deadline = upload_ctx.deadline();
        let stall_after = deadline.map_or(Duration::MAX, |d| {
            let left = d.saturating_duration_since(now);
            left.checked_mul(3).map_or(left / 4 * 3, |x| x / 4)
        });

        let limit = usize::try_from(self.config.gcs_max_concurrent_uploads)
            .ok()
            .filter(|n| *n > 0)
            .unwrap_or(DEFAULT_GCS_MAX_CONCURRENT_UPLOADS as usize);
        let sem = Arc::new(Semaphore::new(limit));
        let mut set = JoinSet::new();
        for (idx, t) in tasks.iter().enumerate() {
            let Ok(permit) = Arc::clone(&sem).acquire_owned().await else {
                break; // never closed
            };
            let upload_ctx = upload_ctx.clone();
            let offloader = Arc::clone(offloader);
            let path = t.path.clone();
            let pending = t.pending.clone();
            set.spawn(async move {
                let _permit = permit;
                let start = Instant::now();
                let result = upload_payload(&upload_ctx, &offloader, &path, &pending).await;
                // Whether the deadline of upload_ctx had passed when the upload returned. An
                // offloader can fail with a deadline error of its own, such as a per-request
                // timeout, before that deadline, which then did not cut the upload short.
                let deadline_passed = upload_ctx.err() == Some(ContextError::DeadlineExceeded);
                let mut cut_short = false;
                let mut stalled = false;
                if result.started
                    && result.err.as_ref().is_some_and(|e| {
                        e.is_context(ContextError::DeadlineExceeded) || deadline_passed
                    })
                {
                    cut_short = true;
                    // Only an upload that the deadline cut short can have stalled until it.
                    // The offloader may return a while after the deadline, which is no time
                    // that the upload spent running.
                    let ran = deadline.map_or(start.elapsed(), |d| {
                        start.elapsed().min(d.saturating_duration_since(start))
                    });
                    stalled = deadline_passed && ran >= stall_after;
                }
                let succeeded = result.err.is_none() && result.uri.starts_with("gs://");
                (idx, result, cut_short, stalled, succeeded)
            });
        }
        let (mut any_started, mut any_succeeded, mut any_cut_short, mut any_stalled) =
            (false, false, false, false);
        while let Some(joined) = set.join_next().await {
            match joined {
                Ok((idx, result, cut_short, stalled, succeeded)) => {
                    any_started |= result.started;
                    any_cut_short |= cut_short;
                    any_stalled |= stalled;
                    any_succeeded |= succeeded;
                    if let Some(t) = tasks.get_mut(idx) {
                        t.uri = result.uri;
                        t.err = result.err;
                    }
                }
                // The offloader's panics are caught in upload_payload; anything else is a bug.
                Err(err) if err.is_panic() => resume_unwind(err.into_panic()),
                Err(_) => {}
            }
        }

        // Pause uploads when at least one upload started and:
        //   - the batch's own timeout ended the uploads and either an in-flight upload timed
        //     out under the batch deadline (brownout) or no upload in the batch succeeded, or
        //   - a caller deadline allowed to pause (opts.pause_on_caller_deadline) ended the
        //     uploads before the batch's own timeout and either no upload in the batch
        //     succeeded or the deadline cut short an upload that had been running for at least
        //     stall_after, even if other uploads in the batch succeeded (partial outage). An
        //     upload that failed with a deadline error of its own before the deadline passed
        //     does not count.
        let own_timeout_expired = upload_ctx
            .cause()
            .is_some_and(|cause| Arc::ptr_eq(&cause, &timeout_cause));
        let batch_timed_out = if own_timeout_expired {
            any_cut_short || !any_succeeded
        } else if opts.pause_on_caller_deadline
            && upload_ctx.err() == Some(ContextError::DeadlineExceeded)
        {
            any_stalled || !any_succeeded
        } else {
            false
        };
        let paused = any_started && batch_timed_out;
        if paused {
            *lock(&self.upload_cooldown_until) = Instant::now().checked_add(upload_timeout);
        } else if any_succeeded {
            *lock(&self.upload_cooldown_until) = None;
        }
        upload_ctx.cancel();
        UploadsOutcome {
            own_timeout_expired,
            paused,
        }
    }

    /// The part of `resolveUploadsForBatch` that applies the results (and may log, so a
    /// Logger may panic in it). Returns Go's `deferredReserved`.
    fn settle_uploads(
        &self,
        ctx: &Context,
        opts: UploadResolveOptions,
        stage: Stage,
        batch: &mut [QueuedRow],
        tasks: &mut [UploadTask],
        summaries: &[(usize, SharedSummaryBuilder)],
    ) -> i64 {
        let outcome = match stage {
            Stage::NoUploads { offloader_missing } => {
                for q in batch.iter_mut() {
                    q.deferred = None;
                }
                if offloader_missing {
                    for t in tasks.iter() {
                        if !t.pending.is_text {
                            continue;
                        }
                        let row = &mut batch[t.row].row;
                        if t.pending.fallback_truncated {
                            row.insert("is_truncated".to_owned(), RowValue::Bool(true));
                        }
                        if let Some(builder) = &t.builder {
                            let fallback_text = part_text(row, t.slot);
                            lock(builder).retain_inline_summary(
                                t.pending.content_ord,
                                t.pending.part_idx,
                                &fallback_text,
                                t.pending.fallback_truncated,
                            );
                        }
                    }
                    apply_summaries(batch, summaries);
                }
                return 0;
            }
            Stage::Cooldown => {
                for q in batch.iter_mut() {
                    q.deferred = None;
                }
                for t in tasks.iter_mut() {
                    t.err = Some(UploadSentinel::UploadCooldown.error());
                    self.apply_upload(&mut batch[t.row].row, t, false);
                }
                apply_summaries(batch, summaries);
                return 0;
            }
            Stage::Uploaded(outcome) => outcome,
        };

        // When opts.can_defer is true and ctx has ended the uploads, rather than the batch's
        // own timeout, every upload that returned an error in the batch is kept for a retry of
        // the batch, unless this batch paused the uploads: the uploads that failed once ctx had
        // ended them, and any that had already failed, so that a custom offloader that returns
        // an opaque error when ctx ends is retried too. An upload that returned no error is not
        // kept, even if its URI is not a gs:// URI.
        let keep_cut_short = opts.can_defer
            && !outcome.own_timeout_expired
            && ctx.err().is_some()
            && !outcome.paused;
        let mut deferred_reserved: i64 = 0;
        for t in tasks.iter() {
            let q = &mut batch[t.row];
            let mut deferred = false;
            if keep_cut_short && let Some(err) = &t.err {
                let event_id = q.row.str(EVENT_ID_FIELD).unwrap_or_default().to_owned();
                if let Some(d) = q.deferred.as_mut() {
                    d.pending.push(DeferredUpload {
                        upload: t.pending.clone(),
                        event_id,
                        err_summary: safe_upload_error_summary(err),
                    });
                    deferred_reserved += t.pending.reserved;
                    deferred = true;
                }
            }
            if !deferred
                && t.err.is_some()
                && t.pending.is_text
                && t.pending.fallback_truncated
                && let Some(d) = q.deferred.as_mut()
            {
                d.non_deferred_truncated = true;
            }
            self.apply_upload(&mut q.row, t, deferred);
        }
        if !keep_cut_short {
            self.clear_deferred_uploads(batch);
        }
        apply_summaries(batch, summaries);
        deferred_reserved
    }

    /// Go `clearDeferredUploads(batch)`: drops the deferred uploads of the rows (releasing the
    /// reservations of all of them first, so that a panicking Logger leaks none) and logs
    /// their failures as final.
    pub(crate) fn clear_deferred_uploads(&self, batch: &mut [QueuedRow]) {
        let dropped = self.drop_deferred_uploads_quietly(batch);
        self.log_deferred_upload_failures(&dropped, false);
    }

    /// Go `dropDeferredUploadsQuietly(batch)`: drops the deferred uploads and releases their
    /// reservations without logging; returns them. On a settled batch it does nothing.
    pub(crate) fn drop_deferred_uploads_quietly(
        &self,
        batch: &mut [QueuedRow],
    ) -> Vec<DeferredUpload> {
        let mut dropped = Vec::new();
        let mut total: i64 = 0;
        for q in batch.iter_mut() {
            let Some(d) = q.deferred.take() else {
                continue;
            };
            total += d.pending.iter().map(|du| du.upload.reserved).sum::<i64>();
            dropped.extend(d.pending);
        }
        self.release_upload_bytes(total);
        dropped
    }

    /// Go `requeueDeferredUploads(batch)`: queues the deferred uploads of the rows again, with
    /// their parts back in the form they were queued in (keeping their reservations), and logs
    /// their failures as pending a retry once every row has been requeued.
    pub(crate) fn requeue_deferred_uploads(&self, batch: &mut [QueuedRow]) {
        let mut requeued = Vec::new();
        for q in batch.iter_mut() {
            let Some(d) = q.deferred.take() else {
                continue;
            };
            if d.pending.is_empty() {
                continue;
            }
            let mut pending = Vec::with_capacity(d.pending.len());
            let mut has_text = false;
            for du in &d.pending {
                let u = &du.upload;
                if !u.is_text
                    && let Some(part) = q
                        .row
                        .parts_mut(COLUMN_CONTENT_PARTS)
                        .and_then(|parts| parts.get_mut(u.slot))
                {
                    part.insert("text".to_owned(), RowValue::from(INLINE_DATA_TEXT));
                }
                if u.is_text {
                    has_text = true;
                    if let Some(builder) = &d.builder {
                        lock(builder).omit_text_from_summary(u.content_ord, u.part_idx);
                    }
                }
                pending.push(u.clone());
            }
            q.raw_parts = pending;
            if let Some(builder) = d.builder {
                if has_text {
                    apply_summary_to_row(&lock(&builder), &mut q.row);
                }
                q.summary_builder = Some(builder);
            } else if has_text {
                q.row.insert(
                    "is_truncated".to_owned(),
                    RowValue::Bool(d.non_deferred_truncated),
                );
            }
            requeued.extend(d.pending);
        }
        self.log_deferred_upload_failures(&requeued, true);
    }

    /// Go `logDeferredUploadFailures(uploads, retry)`.
    pub(crate) fn log_deferred_upload_failures(&self, uploads: &[DeferredUpload], retry: bool) {
        for du in uploads {
            self.log_upload_failure(&du.event_id, &du.upload, &du.err_summary, retry);
        }
    }

    /// Go `objectPath(row, u)`: [`gcs_object_path`] with `gcs_path_prefix`, the row's
    /// timestamp (now when it has none), ids and the payload's extension.
    pub(crate) fn object_path(&self, row: &Row, u: &PendingUpload) -> String {
        let ts = row
            .timestamp(TIMESTAMP_FIELD)
            .filter(|ts| Some(*ts) != go_zero_time())
            .unwrap_or_else(Utc::now);
        gcs_object_path(
            &self.config.gcs_path_prefix,
            ts,
            row.str("trace_id").unwrap_or_default(),
            row.str("span_id").unwrap_or_default(),
            row.str(EVENT_ID_FIELD).unwrap_or_default(),
            u.content_ord,
            u.part_idx,
            &extension_for_mime(&u.mime_type),
        )
    }

    /// Go `applyUpload(t, deferred)` on the task's row (`row` is `batch[t.row].row`): a
    /// successful upload (no error and a `gs://` URI) turns the entry into a
    /// `GCS_REFERENCE` with its `object_ref` (a text keeps a [`text_preview`] and leaves the
    /// summary); otherwise a binary entry gets `[UPLOAD FAILED]`, a text keeps its inline
    /// fallback (and marks the row truncated when that was cut), and the failure is logged
    /// unless the upload is deferred.
    pub(crate) fn apply_upload(&self, row: &mut Row, t: &UploadTask, deferred: bool) {
        let u = &t.pending;
        let invalid_uri = UploadSentinel::InvalidGcsUri.error();
        let err = t
            .err
            .as_ref()
            .or_else(|| (!t.uri.starts_with("gs://")).then_some(&invalid_uri));
        if let Some(err) = err {
            if !u.is_text {
                if let Some(part) = part_mut(row, t.slot) {
                    part.insert("text".to_owned(), RowValue::from(UPLOAD_FAILED_TEXT));
                }
            } else {
                if u.fallback_truncated {
                    row.insert("is_truncated".to_owned(), RowValue::Bool(true));
                }
                if let Some(builder) = &t.builder {
                    let fallback_text = part_text(row, t.slot);
                    lock(builder).retain_inline_summary(
                        u.content_ord,
                        u.part_idx,
                        &fallback_text,
                        u.fallback_truncated,
                    );
                }
            }
            if !deferred {
                let event_id = row.str(EVENT_ID_FIELD).unwrap_or_default();
                self.log_upload_failure(event_id, u, &safe_upload_error_summary(err), false);
            }
            return;
        }
        let details = gcs_metadata_details(u.content_type(), u.size());
        let reference = from_json_map(&object_ref(&t.uri, &details, &self.config.connection_id));
        let Some(part) = part_mut(row, t.slot) else {
            return;
        };
        part.insert(
            "storage_mode".to_owned(),
            RowValue::from(STORAGE_MODE_GCS_REFERENCE),
        );
        part.insert("uri".to_owned(), RowValue::from(t.uri.as_str()));
        part.insert("object_ref".to_owned(), RowValue::Map(reference));
        if u.is_text {
            part.insert("text".to_owned(), RowValue::from(text_preview(&u.text)));
            if let Some(builder) = &t.builder {
                lock(builder).omit_text_from_summary(u.content_ord, u.part_idx);
            }
        } else {
            part.insert("text".to_owned(), RowValue::from(MEDIA_OFFLOADED_TEXT));
        }
    }

    /// Go `logUploadFailure(eventID, u, errSummary, retry)`: the event id, part index, byte
    /// size and `err_summary`, nothing from the payload.
    pub(crate) fn log_upload_failure(
        &self,
        event_id: &str,
        u: &PendingUpload,
        err_summary: &str,
        retry: bool,
    ) {
        if retry {
            self.config.logf(format_args!(
                "agentanalytics: GCS upload failed for event {event_id} part {} ({} bytes): {err_summary}; keeping the part inline pending retry on next flush",
                u.part_idx,
                u.size()
            ));
            return;
        }
        self.config.logf(format_args!(
            "agentanalytics: GCS upload failed for event {event_id} part {} ({} bytes): {err_summary}; keeping the part inline",
            u.part_idx,
            u.size()
        ));
    }
}

/// Go `upload(ctx, offloader, t)`: calls the offloader unless `ctx` has ended; a panic is
/// caught as Go's `offloaderPanicError`. A free function, as the spawned upload tasks cannot
/// borrow the processor.
async fn upload_payload(
    ctx: &Context,
    offloader: &Arc<dyn GcsContentOffloader>,
    path: &str,
    pending: &PendingUpload,
) -> UploadResult {
    if let Some(err) = ctx.err() {
        return UploadResult {
            uri: String::new(),
            started: false,
            err: Some(Error::Context(err)),
        };
    }
    let req = GcsUploadRequest {
        path: path.to_owned(),
        data: pending.payload(),
        content_type: pending.content_type().to_owned(),
    };
    match catch_unwind_future(offloader.upload_content(ctx, req)).await {
        Ok(Ok(uri)) => UploadResult {
            uri,
            started: true,
            err: None,
        },
        Ok(Err(err)) => UploadResult {
            uri: String::new(),
            started: true,
            err: Some(err),
        },
        // The offloader may be supplied by the user, so a panic in it is turned into an error
        // that names only its type. Divergence (ledger 7b-2): Rust has no runtime name of a
        // panic value's type: `string` for `&str` / `String` payloads, else `unknown` (Go
        // `%T`), as 7b-1's content-formatter panics; Rust's panic hook still reports the panic
        // (stderr by default) before it is caught, where Go's `recover` is silent.
        Err(panic) => UploadResult {
            uri: String::new(),
            started: true,
            err: Some(Error::Other(Box::new(OffloaderPanicError {
                value_type: panic_type_name(panic.as_ref()).to_owned(),
            }))),
        },
    }
}

/// A future that catches a panic of the future it wraps (Go's deferred `recover` around a
/// call): resolves to `Err(payload)` when polling it panicked. Used for offloaders (which may
/// be the user's) and by the tests that recover Go's panics.
pub(crate) struct CatchUnwind<F>(Pin<Box<F>>);

impl<F: Future> Future for CatchUnwind<F> {
    type Output = Result<F::Output, Box<dyn Any + Send>>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Self::Output> {
        let fut = self.0.as_mut();
        match catch_unwind(AssertUnwindSafe(move || fut.poll(cx))) {
            Ok(Poll::Pending) => Poll::Pending,
            Ok(Poll::Ready(v)) => Poll::Ready(Ok(v)),
            Err(panic) => Poll::Ready(Err(panic)),
        }
    }
}

/// Wraps `fut` in [`CatchUnwind`].
pub(crate) fn catch_unwind_future<F: Future>(fut: F) -> CatchUnwind<F> {
    CatchUnwind(Box::pin(fut))
}

/// A `std::sync::Mutex` lock that ignores poisoning (a panicking Logger may poison a summary
/// builder's lock; Go has no poisoning).
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Go's zero `time.Time` (`0001-01-01 00:00:00 UTC`), which `objectPath` replaces with now.
fn go_zero_time() -> Option<DateTime<Utc>> {
    NaiveDate::from_ymd_opt(1, 1, 1)
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .map(|d| d.and_utc())
}

/// The `content_parts` entry at `slot` (Go `t.part`).
fn part_mut(row: &mut Row, slot: usize) -> Option<&mut Row> {
    row.parts_mut(COLUMN_CONTENT_PARTS)
        .and_then(|parts| parts.get_mut(slot))
}

/// Go `fallbackText, _ := t.part["text"].(string)`.
fn part_text(row: &Row, slot: usize) -> String {
    row.parts(COLUMN_CONTENT_PARTS)
        .and_then(|parts| parts.get(slot))
        .and_then(|part| part.str("text"))
        .unwrap_or_default()
        .to_owned()
}

/// Go `s.builder.applyToRow(s.row)` for each row summary.
fn apply_summaries(batch: &mut [QueuedRow], summaries: &[(usize, SharedSummaryBuilder)]) {
    for (i, builder) in summaries {
        if let Some(q) = batch.get_mut(*i) {
            apply_summary_to_row(&lock(builder), &mut q.row);
        }
    }
}

/// Go `gcsMetadataDetails(contentType, sizeBytes)`: Go `json.Marshal` of
/// `{"gcs_metadata":{"content_type":…,"size":…}}`.
pub(crate) fn gcs_metadata_details(content_type: &str, size_bytes: usize) -> String {
    let value = json!({"gcs_metadata": {"content_type": content_type, "size": size_bytes}});
    gadk_model::to_go_json(&value)
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
        .unwrap_or_else(|| "{}".to_owned())
}

/// Go `textPreview(text)`: the first 200 chars and `... [OFFLOADED]` (a `&str` is valid
/// UTF-8, so Go's `strings.ToValidUTF8` has nothing to do).
pub(crate) fn text_preview(text: &str) -> String {
    let cut = text
        .char_indices()
        .nth(TEXT_PREVIEW_RUNES)
        .map_or(text, |(i, _)| &text[..i]);
    format!("{cut}{TEXT_PREVIEW_SUFFIX}")
}

/// Go's built-in MIME table (`mime.builtinTypesLower`, Go 1.26) by type, for the types
/// [`MIME_EXTENSIONS`] does not fix: the first of each type's sorted extensions (all safe),
/// which is what Go's `mime.ExtensionsByType` answers first on a host without MIME tables.
const GO_BUILTIN_MIME_EXTENSIONS: [(&str, &str); 35] = [
    ("application/gzip", ".gz"),
    ("application/msword", ".doc"),
    ("application/octet-stream", ".bin"),
    ("application/postscript", ".ai"),
    ("application/rdf+xml", ".rdf"),
    ("application/rtf", ".rtf"),
    ("application/vnd.android.package-archive", ".apk"),
    ("application/vnd.ms-excel", ".xls"),
    ("application/vnd.ms-powerpoint", ".ppt"),
    (
        "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        ".pptx",
    ),
    (
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        ".xlsx",
    ),
    (
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        ".docx",
    ),
    ("application/wasm", ".wasm"),
    ("application/xhtml+xml", ".xht"),
    ("application/zip", ".zip"),
    ("audio/flac", ".flac"),
    ("audio/mp4", ".m4a"),
    ("audio/ogg", ".oga"),
    ("audio/webm", ".webm"),
    ("image/apng", ".apng"),
    ("image/avif", ".avif"),
    ("image/bmp", ".bmp"),
    ("image/svg+xml", ".svg"),
    ("image/tiff", ".tif"),
    ("image/vnd.microsoft.icon", ".ico"),
    ("image/x-xbitmap", ".xbm"),
    ("message/rfc822", ".eml"),
    ("text/calendar", ".ics"),
    ("text/css", ".css"),
    ("text/csv", ".csv"),
    ("text/html", ".ehtml"),
    ("text/javascript", ".js"),
    ("text/vtt", ".vtt"),
    ("text/xml", ".xbl"),
    ("video/ogg", ".ogv"),
];

/// Go `extensionForMIME(mimeType)`: the extension, with its dot, of objects of the type;
/// parameters, case and blanks are ignored; [`FALLBACK_EXTENSION`] for a type without one.
pub(crate) fn extension_for_mime(mime_type: &str) -> String {
    let base = mime_type
        .split_once(';')
        .map_or(mime_type, |(b, _)| b)
        .trim()
        .to_lowercase();
    if let Ok(i) = MIME_EXTENSIONS.binary_search_by(|(t, _)| (*t).cmp(base.as_str())) {
        return MIME_EXTENSIONS[i].1.to_owned();
    }
    // Divergence (ledger 7b-2): Go asks `mime.ExtensionsByType`, whose answer also comes from
    // the host's MIME tables (`/etc/mime.types` and the like) and `mime.AddExtensionType`;
    // gadk answers from Go's built-in table alone, so a type that only the host knows gets
    // `.bin`.
    GO_BUILTIN_MIME_EXTENSIONS
        .binary_search_by(|(t, _)| (*t).cmp(base.as_str()))
        .ok()
        .map(|i| GO_BUILTIN_MIME_EXTENSIONS[i].1)
        .filter(|ext| is_safe_extension(ext))
        .unwrap_or(FALLBACK_EXTENSION)
        .to_owned()
}

/// Go `isSafeExtension(ext)`: a dot and 1 to 16 ASCII letters or digits.
pub(crate) fn is_safe_extension(ext: &str) -> bool {
    if ext.len() < 2 || ext.len() > 17 || !ext.starts_with('.') {
        return false;
    }
    ext[1..].chars().all(|c| c.is_ascii_alphanumeric())
}

/// Go `pathSegment(id, fallback)`: `id` with every char but ASCII letters, digits, `-`, `_`
/// and `.` replaced by `_`, or `fallback` when `id` is empty or only dots.
pub(crate) fn path_segment(id: &str, fallback: &str) -> String {
    if id.trim_matches('.').is_empty() {
        return fallback.to_owned();
    }
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Go `gcsObjectPath(prefix, ts, traceID, spanID, eventID, contentOrd, partIdx, ext)`:
/// `[prefix/]<UTC date>/<trace id>/<span id>_<event id>_c<content>_p<part><ext>`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn gcs_object_path(
    prefix: &str,
    ts: DateTime<Utc>,
    trace_id: &str,
    span_id: &str,
    event_id: &str,
    content_ord: usize,
    part_idx: usize,
    ext: &str,
) -> String {
    let mut out = String::new();
    let p = prefix.trim_matches('/');
    if !p.is_empty() {
        out.push_str(p);
        out.push('/');
    }
    out.push_str(&format!(
        "{}/{}/{}_{}_c{content_ord}_p{part_idx}{ext}",
        ts.format("%Y-%m-%d"),
        path_segment(trace_id, MISSING_TRACE_ID),
        path_segment(span_id, MISSING_SPAN_ID),
        path_segment(event_id, MISSING_EVENT_ID),
    ));
    out
}

/// Go `safeUploadErrorSummary(err)`: diagnostic metadata of `err` that never shows a custom
/// offloader's error text: the context errors and upload sentinels by name, an offloader
/// panic's type, the built-in offloader's own error scrubbed ([`redact_error_message`], then
/// [`strip_uri_queries`]), a Google API error's HTTP status, else the error's Go type.
pub(crate) fn safe_upload_error_summary(err: &Error) -> String {
    if err.is_context(ContextError::DeadlineExceeded) {
        return ContextError::DeadlineExceeded.to_string();
    }
    if err.is_context(ContextError::Canceled) {
        return ContextError::Canceled.to_string();
    }
    for sentinel in [
        UploadSentinel::InvalidGcsUri,
        UploadSentinel::UploadCooldown,
    ] {
        if sentinel.is_in(err) {
            return sentinel.to_string();
        }
    }
    if let Some(Error::Other(b)) =
        err.find(&mut |e| matches!(e, Error::Other(b) if b.is::<OffloaderPanicError>()))
        && let Some(panic) = b.downcast_ref::<OffloaderPanicError>()
    {
        return panic.to_string();
    }
    if let Some(owned) = OwnedUploadError::find(err) {
        return strip_uri_queries(&redact_error_message(&owned.to_string()));
    }
    if let Some(Error::Api(api)) = err.find(&mut |e| matches!(e, Error::Api(_))) {
        return format!("HTTP {}", api.code);
    }
    format!("error type {}", go_error_type(err))
}

/// Go `%T` of an error, by the [`Error`] variant that carries what Go's error type was.
/// Divergence (ledger 7b-2): an `Error::Other` (a custom offloader's own error type) or an
/// Arrow error is `error` — Rust has no runtime type name for a `Box<dyn Error>` (Go names
/// the dynamic type, such as `agentanalytics.zzCustomErr`).
fn go_error_type(err: &Error) -> &'static str {
    match err {
        Error::Message(_) => "*errors.errorString",
        Error::Wrapped { .. } => "*fmt.wrapError",
        Error::Joined(_) => "*errors.joinError",
        Error::Context(ContextError::Canceled) => "*errors.errorString",
        Error::Context(ContextError::DeadlineExceeded) => "context.deadlineExceededError",
        Error::Status { .. } => "*status.Error",
        Error::Api(_) => "*googleapi.Error",
        Error::BigQuery(_) => "*bigquery.Error",
        Error::SchemaMismatch(_) => "*agentanalytics.schemaMismatchError",
        Error::Rest(_) => "*url.Error",
        Error::Arrow(_) | Error::Other(_) => "error",
    }
}

/// Go `stripURIQueries(msg)`: the URIs in `msg` without their query strings and fragments.
pub(crate) fn strip_uri_queries(msg: &str) -> String {
    if !msg.contains("://") {
        return msg.to_owned();
    }
    URI_TOKEN_RE
        .replace_all(msg, |caps: &regex::Captures<'_>| {
            let token = caps.get(0).map_or("", |m| m.as_str());
            let mut trimmed = trim_trailing_punctuation(token);
            let suffix = &token[trimmed.len()..];
            if let Some(i) = trimmed.find(['?', '#']) {
                trimmed = &trimmed[..i];
            }
            format!("{trimmed}{suffix}")
        })
        .into_owned()
}

#[cfg(test)]
mod tests {
    //! The pure helpers against the Go probe's tables (`tests/fixtures/helpers/*.json`).
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use std::path::PathBuf;

    use pretty_assertions::assert_eq;
    use serde_json::Value;

    use super::*;
    use crate::config::Config;
    use crate::error::ApiError;

    fn cases(name: &str) -> Vec<Value> {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/helpers")
            .join(format!("{name}.json"));
        let go: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        go["cases"].as_array().unwrap().clone()
    }

    fn s<'a>(v: &'a Value, key: &str) -> &'a str {
        v[key].as_str().unwrap_or_else(|| panic!("{key} in {v}"))
    }

    #[test]
    fn extension_for_mime_matches_go() {
        let cases = cases("extension_for_mime");
        assert_eq!(cases.len(), 21);
        for c in &cases {
            assert_eq!(extension_for_mime(s(c, "mime_type")), s(c, "ext"), "{c}");
        }
    }

    #[test]
    fn is_safe_extension_matches_go() {
        for c in &cases("is_safe_extension") {
            assert_eq!(
                is_safe_extension(s(c, "ext")),
                c["safe"].as_bool().unwrap(),
                "{c}"
            );
        }
    }

    #[test]
    fn path_segment_matches_go() {
        for c in &cases("path_segment") {
            assert_eq!(
                path_segment(s(c, "id"), s(c, "fallback")),
                s(c, "result"),
                "{c}"
            );
        }
    }

    #[test]
    fn text_preview_matches_go() {
        for c in &cases("text_preview") {
            assert_eq!(
                text_preview(s(c, "text")),
                s(c, "preview"),
                "{}",
                s(c, "name")
            );
        }
    }

    /// Go's negative `int` sizes and ordinals have no `usize` (the probe's `-1` cases are left
    /// out; gadk's sizes and ordinals cannot be negative).
    fn usize_of(v: &Value) -> Option<usize> {
        v.as_u64().and_then(|n| usize::try_from(n).ok())
    }

    #[test]
    fn gcs_metadata_details_matches_go() {
        let mut checked = 0;
        for c in &cases("gcs_metadata_details") {
            let Some(size) = usize_of(&c["size"]) else {
                continue;
            };
            assert_eq!(
                gcs_metadata_details(s(c, "content_type"), size),
                s(c, "details"),
                "{c}"
            );
            checked += 1;
        }
        assert_eq!(checked, 4);
    }

    #[test]
    fn gcs_object_path_matches_go() {
        let mut checked = 0;
        for c in &cases("gcs_object_path") {
            let a = &c["args"];
            let (Some(content_ord), Some(part_idx)) =
                (usize_of(&a["content_ord"]), usize_of(&a["part_idx"]))
            else {
                continue;
            };
            let ts = DateTime::parse_from_rfc3339(s(a, "ts"))
                .unwrap()
                .with_timezone(&Utc);
            let got = gcs_object_path(
                s(a, "prefix"),
                ts,
                s(a, "trace_id"),
                s(a, "span_id"),
                s(a, "event_id"),
                content_ord,
                part_idx,
                s(a, "ext"),
            );
            assert_eq!(got, s(c, "path"), "{c}");
            checked += 1;
        }
        assert_eq!(checked, 12);
    }

    #[test]
    fn object_path_matches_go() {
        for c in &cases("object_path") {
            let bp = BatchProcessor::bare(
                Config {
                    gcs_path_prefix: s(c, "gcs_path_prefix").to_owned(),
                    ..Config::zero()
                },
                0,
            );
            let row = crate::testutil::row_from_spec(&c["row"]).unwrap();
            let u = &c["upload"];
            let upload = PendingUpload {
                content_ord: usize_of(&u["content_ord"]).unwrap(),
                part_idx: usize_of(&u["part_idx"]).unwrap(),
                mime_type: s(u, "mime_type").to_owned(),
                ..PendingUpload::default()
            };
            assert_eq!(
                bp.object_path(&row, &upload),
                s(c, "path"),
                "{}",
                s(c, "name")
            );
        }
    }

    #[test]
    fn strip_uri_queries_matches_go() {
        let cases = cases("strip_uri_queries");
        assert_eq!(cases.len(), 10);
        for c in &cases {
            assert_eq!(strip_uri_queries(s(c, "input")), s(c, "output"), "{c}");
        }
    }

    /// A custom offloader's own error type (Go `zzCustomErr`).
    #[derive(Debug)]
    struct CustomErr;

    impl std::fmt::Display for CustomErr {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("custom secret")
        }
    }

    impl std::error::Error for CustomErr {}

    fn api(code: u16, message: &str) -> Error {
        Error::Api(Box::new(ApiError {
            code,
            message: message.to_owned(),
            ..ApiError::default()
        }))
    }

    fn panicked(value_type: &str) -> Error {
        Error::Other(Box::new(OffloaderPanicError {
            value_type: value_type.to_owned(),
        }))
    }

    /// The gadk error for each probed Go error (by case name).
    fn error_for(name: &str) -> Error {
        const WRITE: &str = "failed to write the Cloud Storage object";
        match name {
            "deadline" => Error::Context(ContextError::DeadlineExceeded),
            "deadline_wrapped" => {
                Error::wrap("upload", Error::Context(ContextError::DeadlineExceeded))
            }
            "batch_upload_timeout" => UploadSentinel::BatchUploadTimeout.error(),
            "canceled" => Error::Context(ContextError::Canceled),
            "invalid_gcs_uri" => UploadSentinel::InvalidGcsUri.error(),
            "upload_cooldown" => UploadSentinel::UploadCooldown.error(),
            "panic" => panicked("string"),
            "panic_wrapped" => Error::wrap("x", panicked("*errors.errorString")),
            "owned_api_error" => OwnedUploadError::wrap(Error::wrap(
                WRITE,
                api(
                    403,
                    "forbidden https://storage.googleapis.com/b/o?token=abc",
                ),
            )),
            "owned_bearer" => OwnedUploadError::wrap(Error::msg(format!(
                "{WRITE}: Authorization: Bearer ya29.secret-token at https://h/p?sig=1"
            ))),
            "owned_deadline" => OwnedUploadError::wrap(Error::wrap(
                WRITE,
                Error::Context(ContextError::DeadlineExceeded),
            )),
            "api_error" => api(503, ""),
            "api_error_wrapped" => Error::wrap("custom", api(429, "secret")),
            "plain_error" => Error::msg("secret detail"),
            "wrapped_plain_error" => Error::wrap("secret", Error::msg("x")),
            "custom_type" => Error::Other(Box::new(CustomErr)),
            other => panic!("unknown case {other}"),
        }
    }

    #[test]
    fn safe_upload_error_summary_matches_go() {
        let cases = cases("safe_upload_error_summary");
        assert_eq!(cases.len(), 16);
        for c in &cases {
            let name = s(c, "name");
            let err = error_for(name);
            assert_eq!(err.to_string(), s(c, "error_text"), "{name}: error text");
            let want = if name == "custom_type" {
                // The ledgered divergence of `go_error_type`: no runtime type name.
                "error type error"
            } else {
                s(c, "summary")
            };
            assert_eq!(safe_upload_error_summary(&err), want, "{name}");
        }
    }
}
