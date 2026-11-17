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

//! The upload orchestration of a batch (`impl BatchProcessor`): `resolve_uploads_for_batch` with
//! its concurrency limit, batch timeout (told apart from the caller's deadline by the context's
//! cause), cooldown and deferred uploads (`clear_deferred_uploads`,
//! `drop_deferred_uploads_quietly`, `requeue_deferred_uploads`, `log_deferred_upload_failures`),
//! `object_path`, `upload_payload` (offloader panics caught), `apply_upload`,
//! `log_upload_failure`, and the pure helpers (`gcs_metadata_details`, `text_preview`,
//! `extension_for_mime`, `is_safe_extension`, `path_segment`, `gcs_object_path`,
//! `safe_upload_error_summary`, `strip_uri_queries`).

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

use sha2::{Digest, Sha256};

use super::{
    FALLBACK_EXTENSION, MAX_GCS_OBJECT_NAME_BYTES, MAX_GCS_PREFIX_BYTES,
    MAX_GCS_REWRITTEN_PREFIX_BYTES, MAX_PATH_SEGMENT_BYTES, MEDIA_OFFLOADED_TEXT, MIME_EXTENSIONS,
    MISSING_EVENT_ID, MISSING_SPAN_ID, MISSING_TRACE_ID, OffloaderPanicError, OwnedUploadError,
    PATH_SEGMENT_HASH_SUFFIX_LEN, TEXT_PREVIEW_RUNES, TEXT_PREVIEW_SUFFIX, UploadSentinel,
};
use crate::batch_processor::BatchProcessor;
use crate::config::{
    Config, DEFAULT_GCS_MAX_CONCURRENT_UPLOADS, GCS_UPLOAD_TIMEOUT, GcsContentOffloader,
    GcsUploadRequest,
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
use crate::views::ascii_equal_fold;

/// The `event_id` field name.
const EVENT_ID_FIELD: &str = "event_id";

/// The `timestamp` field name.
const TIMESTAMP_FIELD: &str = "timestamp";

/// How `resolve_uploads_for_batch` resolves the uploads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct UploadResolveOptions {
    /// Pause the uploads when the caller's deadline ends them.
    pub(crate) pause_on_caller_deadline: bool,
    /// Whether failed uploads may be deferred for a later requeue.
    pub(crate) can_defer: bool,
}

/// One payload to upload and the `content_parts` entry it rewrites, located by index (the batch
/// row and the entry's slot).
#[derive(Debug)]
pub(crate) struct UploadTask {
    /// The index of the row in the batch.
    pub(crate) row: usize,
    /// The index of the entry in the row's `content_parts` (also the pending upload's slot).
    pub(crate) slot: usize,
    /// The summary builder of the row, if any.
    pub(crate) builder: Option<SharedSummaryBuilder>,
    /// The pending upload.
    pub(crate) pending: PendingUpload,
    /// The object name.
    pub(crate) path: String,
    /// The resulting URI.
    pub(crate) uri: String,
    /// The upload's error, if it failed.
    pub(crate) err: Option<Error>,
}

/// What an upload returns: `(uri, started, err)`.
#[derive(Debug)]
pub(crate) struct UploadResult {
    /// The resulting URI.
    pub(crate) uri: String,
    /// The offloader was called.
    pub(crate) started: bool,
    /// The upload's error, if it failed.
    pub(crate) err: Option<Error>,
}

/// How the uploads of a batch ended (the state `resolve_uploads_for_batch` keeps across waiting
/// for its tasks).
#[derive(Debug, Clone, Copy)]
struct UploadsOutcome {
    /// The batch's own timeout expired.
    own_timeout_expired: bool,
    /// The uploads were paused.
    paused: bool,
}

/// Which way `resolve_uploads_for_batch` went.
enum Stage {
    /// No task or no offloader (the first early return).
    NoUploads { offloader_missing: bool },
    /// The uploads are paused (the cooldown early return).
    Cooldown,
    /// The uploads ran.
    Uploaded(UploadsOutcome),
}

impl BatchProcessor {
    /// Uploads the payloads queued with the rows of `batch` (at most
    /// `gcs_max_concurrent_uploads` at a time, under one batch timeout that `ctx` can end
    /// sooner) and rewrites the matching `content_parts` entries; a failed upload keeps the part
    /// inline. A batch whose uploads show Cloud Storage stalled pauses the uploads for the
    /// length of the timeout. With `opts.can_defer`, when `ctx` (not the batch timeout) ended
    /// the uploads without pausing them, the failed uploads stay reserved on their rows'
    /// [`DeferredRowUploads`] for [`requeue_deferred_uploads`] or [`clear_deferred_uploads`];
    /// otherwise no row keeps any.
    ///
    /// A [`Logger`](crate::config::Logger) that panics while the results are applied unwinds
    /// through this function after every reservation it took over is released and every
    /// row's deferred uploads are dropped.
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

    /// The batch upload timeout: the processor's, else the config's, else the default.
    fn batch_upload_timeout(&self) -> Duration {
        [
            self.upload_timeout_override(),
            self.config.gcs_upload_timeout,
        ]
        .into_iter()
        .find(|d| !d.is_zero())
        .unwrap_or(GCS_UPLOAD_TIMEOUT)
    }

    /// The uploads of `resolve_uploads_for_batch`, from the batch timeout's start to the pause
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

    /// The part of `resolve_uploads_for_batch` that applies the results (and may log, so a
    /// Logger may panic in it). Returns whether deferred uploads stayed reserved.
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

    /// Drops the deferred uploads of the rows (releasing the reservations of all of them first,
    /// so that a panicking Logger leaks none) and logs their failures as final.
    pub(crate) fn clear_deferred_uploads(&self, batch: &mut [QueuedRow]) {
        let dropped = self.drop_deferred_uploads_quietly(batch);
        self.log_deferred_upload_failures(&dropped, false);
    }

    /// Drops the deferred uploads and releases their reservations without logging; returns
    /// them. On a settled batch it does nothing.
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

    /// Queues the deferred uploads of the rows again, with their parts back in the form they
    /// were queued in (keeping their reservations), and logs their failures as pending a retry
    /// once every row has been requeued.
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

    /// Logs the failures of `uploads`; `retry` says they are pending a retry.
    pub(crate) fn log_deferred_upload_failures(&self, uploads: &[DeferredUpload], retry: bool) {
        for du in uploads {
            self.log_upload_failure(&du.event_id, &du.upload, &du.err_summary, retry);
        }
    }

    /// [`gcs_object_path_from_clean_prefix`] with the effective prefix resolved at construction
    /// (a processor built without the constructor, such as in a test, cleans the configured
    /// prefix on each call), the row's timestamp (now when it has none), ids and the payload's
    /// extension.
    pub(crate) fn object_path(&self, row: &Row, u: &PendingUpload) -> String {
        let ts = row
            .timestamp(TIMESTAMP_FIELD)
            .filter(|ts| Some(*ts) != go_zero_time())
            .unwrap_or_else(Utc::now);
        let clean_prefix = match &self.effective_gcs_path_prefix {
            Some(p) => std::borrow::Cow::Borrowed(p.as_str()),
            None => std::borrow::Cow::Owned(clean_gcs_prefix(&self.config.gcs_path_prefix)),
        };
        gcs_object_path_from_clean_prefix(
            &clean_prefix,
            ts,
            row.str("trace_id").unwrap_or_default(),
            row.str("span_id").unwrap_or_default(),
            row.str(EVENT_ID_FIELD).unwrap_or_default(),
            u.content_ord,
            u.part_idx,
            &extension_for_mime(&u.mime_type),
        )
    }

    /// Applies the upload `t` to its row (`row` is `batch[t.row].row`): a successful upload (no
    /// error and a `gs://` URI) turns the entry into a `GCS_REFERENCE` with its `object_ref` (a
    /// text keeps a [`text_preview`] and leaves the summary); otherwise a binary entry gets
    /// `[UPLOAD FAILED]`, a text keeps its inline fallback (and marks the row truncated when
    /// that was cut), and the failure is logged unless the upload is deferred.
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

    /// Logs a failed upload: the event id, part index, byte size and `err_summary`, nothing from
    /// the payload.
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

/// Calls the offloader unless `ctx` has ended; a panic is caught as an
/// [`OffloaderPanicError`]. A free function, as the spawned upload tasks cannot borrow the
/// processor.
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
        // that names only its type. Rust has no runtime name of a panic value's type: it is
        // `string` for `&str` / `String` payloads, else `unknown`, as for content-formatter
        // panics; Rust's panic hook still reports the panic (stderr by default) before it is
        // caught.
        Err(panic) => UploadResult {
            uri: String::new(),
            started: true,
            err: Some(Error::Other(Box::new(OffloaderPanicError {
                value_type: panic_type_name(panic.as_ref()).to_owned(),
            }))),
        },
    }
}

/// A future that catches a panic of the future it wraps: resolves to `Err(payload)` when
/// polling it panicked. Used for offloaders (which may be the user's) and by the tests that
/// recover panics.
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
/// builder's lock).
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The zero timestamp (`0001-01-01 00:00:00 UTC`), which `object_path` replaces with now.
fn go_zero_time() -> Option<DateTime<Utc>> {
    NaiveDate::from_ymd_opt(1, 1, 1)
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .map(|d| d.and_utc())
}

/// The `content_parts` entry at `slot`.
fn part_mut(row: &mut Row, slot: usize) -> Option<&mut Row> {
    row.parts_mut(COLUMN_CONTENT_PARTS)
        .and_then(|parts| parts.get_mut(slot))
}

/// The `text` of the `content_parts` entry at `slot`, or "".
fn part_text(row: &Row, slot: usize) -> String {
    row.parts(COLUMN_CONTENT_PARTS)
        .and_then(|parts| parts.get(slot))
        .and_then(|part| part.str("text"))
        .unwrap_or_default()
        .to_owned()
}

/// Applies each row summary builder to its row.
fn apply_summaries(batch: &mut [QueuedRow], summaries: &[(usize, SharedSummaryBuilder)]) {
    for (i, builder) in summaries {
        if let Some(q) = batch.get_mut(*i) {
            apply_summary_to_row(&lock(builder), &mut q.row);
        }
    }
}

/// The JSON text `{"gcs_metadata":{"content_type":…,"size":…}}`.
pub(crate) fn gcs_metadata_details(content_type: &str, size_bytes: usize) -> String {
    let value = json!({"gcs_metadata": {"content_type": content_type, "size": size_bytes}});
    gadk_model::to_go_json(&value)
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
        .unwrap_or_else(|| "{}".to_owned())
}

/// The first 200 chars and `... [OFFLOADED]` (a `&str` is valid UTF-8, so no invalid sequence
/// needs replacing).
pub(crate) fn text_preview(text: &str) -> String {
    let cut = text
        .char_indices()
        .nth(TEXT_PREVIEW_RUNES)
        .map_or(text, |(i, _)| &text[..i]);
    format!("{cut}{TEXT_PREVIEW_SUFFIX}")
}

/// Extensions of further common MIME types (such as `application/gzip` → `.gz`) that
/// [`MIME_EXTENSIONS`] does not fix, sorted by type: for each type, the alphabetically first of
/// its standard extensions (all safe).
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

/// The extension, with its dot, of objects of the type; parameters, case and blanks are
/// ignored; [`FALLBACK_EXTENSION`] for a type without one.
pub(crate) fn extension_for_mime(mime_type: &str) -> String {
    let base = mime_type
        .split_once(';')
        .map_or(mime_type, |(b, _)| b)
        .trim()
        .to_lowercase();
    if let Ok(i) = MIME_EXTENSIONS.binary_search_by(|(t, _)| (*t).cmp(base.as_str())) {
        return MIME_EXTENSIONS[i].1.to_owned();
    }
    // The answer comes from the standard table alone, not from the host's MIME tables
    // (`/etc/mime.types` and the like), so a type that only the host knows gets `.bin`.
    GO_BUILTIN_MIME_EXTENSIONS
        .binary_search_by(|(t, _)| (*t).cmp(base.as_str()))
        .ok()
        .map(|i| GO_BUILTIN_MIME_EXTENSIONS[i].1)
        .filter(|ext| is_safe_extension(ext))
        .unwrap_or(FALLBACK_EXTENSION)
        .to_owned()
}

/// Whether `ext` is a dot and 1 to 16 ASCII letters or digits.
pub(crate) fn is_safe_extension(ext: &str) -> bool {
    if ext.len() < 2 || ext.len() > 17 || !ext.starts_with('.') {
        return false;
    }
    ext[1..].chars().all(|c| c.is_ascii_alphanumeric())
}

/// `~` and the 64 lowercase hex digits of the SHA-256 digest of `s`. `~` lies outside the
/// unchanged identifier alphabet (`[A-Za-z0-9._-]`), so hashed outputs occupy a namespace of
/// their own.
pub(crate) fn path_segment_hash_suffix(s: &str) -> String {
    use std::fmt::Write as _;
    let sum = Sha256::digest(s.as_bytes());
    let mut out = String::with_capacity(PATH_SEGMENT_HASH_SUFFIX_LEN);
    out.push('~');
    for b in sum {
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// `base` cut to at most `max_bytes - PATH_SEGMENT_HASH_SUFFIX_LEN` bytes on a char boundary,
/// followed by [`path_segment_hash_suffix`] of `raw`. Callers pass
/// `max_bytes ≥ PATH_SEGMENT_HASH_SUFFIX_LEN`.
fn truncate_with_hash(base: &str, raw: &str, max_bytes: usize) -> String {
    let suffix = path_segment_hash_suffix(raw);
    let mut limit = max_bytes.saturating_sub(suffix.len());
    let mut base = base;
    if base.len() > limit {
        while limit > 0 && !base.is_char_boundary(limit) {
            limit -= 1;
        }
        base = &base[..limit];
    }
    format!("{base}{suffix}")
}

/// An id of at most [`MAX_PATH_SEGMENT_BYTES`] ASCII letters, digits, `-`, `_` and `.` that is
/// not only dots is returned unchanged; an empty id returns `fallback`; an id of only dots
/// returns `fallback` with the `~<sha256>` suffix of `id`; any other id has every char outside
/// that set (including `~`) replaced by `_` and is cut to fit [`MAX_PATH_SEGMENT_BYTES`] with
/// the `~<sha256>` suffix of the raw id appended.
pub(crate) fn path_segment(id: &str, fallback: &str) -> String {
    if id.is_empty() {
        return fallback.to_owned();
    }
    if id.trim_matches('.').is_empty() {
        return truncate_with_hash(fallback, id, MAX_PATH_SEGMENT_BYTES);
    }
    let mut altered = false;
    let sanitized: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                altered = true;
                '_'
            }
        })
        .collect();
    if !altered && sanitized.len() <= MAX_PATH_SEGMENT_BYTES {
        return sanitized;
    }
    truncate_with_hash(&sanitized, id, MAX_PATH_SEGMENT_BYTES)
}

/// Whether `r` is a control character (U+0000–U+001F, U+007F–U+009F) or the Unicode line /
/// paragraph separator (U+2028, U+2029).
pub(crate) fn is_unsafe_gcs_prefix_rune(r: char) -> bool {
    r < '\u{20}' || ('\u{7f}'..='\u{9f}').contains(&r) || r == '\u{2028}' || r == '\u{2029}'
}

/// Whether the first two `/`-separated segments of `s` are, case-insensitively, `.well-known`
/// and `acme-challenge`.
fn has_reserved_acme_prefix(s: &str) -> bool {
    let Some((seg0, rest)) = s.split_once('/') else {
        return false;
    };
    if !ascii_equal_fold(".well-known", seg0) {
        return false;
    }
    let seg1 = rest.split_once('/').map_or(rest, |(seg1, _)| seg1);
    ascii_equal_fold("acme-challenge", seg1)
}

/// The object name prefix for a configured `gcs_path_prefix` (see
/// [`Config::gcs_path_prefix`](crate::config::Config::gcs_path_prefix)).
pub(crate) fn clean_gcs_prefix(prefix: &str) -> String {
    clean_gcs_prefix_with_reason(prefix).0
}

/// The effective prefix and, when it was rewritten, the `; `-joined reasons. A Rust `&str` is
/// valid UTF-8, so an "invalid UTF-8" reason cannot occur.
pub(crate) fn clean_gcs_prefix_with_reason(prefix: &str) -> (String, String) {
    let trimmed = prefix.trim_matches('/');
    if trimmed.is_empty() {
        return (String::new(), String::new());
    }
    let mut has_control = false;
    let mut has_tilde = false;
    let mut cleaned: String = trimmed
        .chars()
        .map(|r| {
            if r == '~' {
                has_tilde = true;
                '_'
            } else if is_unsafe_gcs_prefix_rune(r) {
                has_control = true;
                '_'
            } else {
                r
            }
        })
        .collect();
    let has_reserved_acme = has_reserved_acme_prefix(&cleaned);
    if has_reserved_acme {
        let rest = cleaned
            .split_once('/')
            .map(|(_, rest)| rest.to_owned())
            .unwrap_or_default();
        cleaned = format!("_well-known/{rest}");
    }
    let mut reasons: Vec<String> = Vec::new();
    if has_control {
        reasons.push("contains control or line-separator character".to_owned());
    }
    if has_tilde {
        reasons.push("contains reserved '~'".to_owned());
    }
    if has_reserved_acme {
        reasons.push("starts with reserved .well-known/acme-challenge prefix".to_owned());
    }
    if trimmed.len() > MAX_GCS_PREFIX_BYTES {
        reasons.push(format!("exceeds {MAX_GCS_PREFIX_BYTES} bytes"));
    }
    if reasons.is_empty() {
        return (cleaned, String::new());
    }
    (
        truncate_with_hash(&cleaned, trimmed, MAX_GCS_REWRITTEN_PREFIX_BYTES),
        reasons.join("; "),
    )
}

/// [`clean_gcs_prefix`] of `c.gcs_path_prefix`, logging a constant-format warning with the
/// rewrite reason and byte counts (never the prefix text) when it had to be rewritten.
pub(crate) fn resolve_gcs_path_prefix(c: &Config) -> String {
    let trimmed = c.gcs_path_prefix.trim_matches('/');
    let (cleaned, reason) = clean_gcs_prefix_with_reason(&c.gcs_path_prefix);
    if !reason.is_empty() {
        c.logf(format_args!(
            "agentanalytics: WARNING: Config.GCSPathPrefix ({} raw bytes, {} trimmed bytes) was rewritten ({} effective bytes): {reason}",
            c.gcs_path_prefix.len(),
            trimmed.len(),
            cleaned.len(),
        ));
    }
    cleaned
}

/// `ext` with every [`is_unsafe_gcs_prefix_rune`] char, `/`, `\\` and `~` replaced by `_` (a
/// Rust `&str` has no invalid bytes to replace).
fn sanitize_extension(ext: &str) -> String {
    ext.chars()
        .map(|r| {
            if is_unsafe_gcs_prefix_rune(r) || matches!(r, '/' | '\\' | '~') {
                '_'
            } else {
                r
            }
        })
        .collect()
}

/// `[prefix/]<UTC date>/<trace id>/<span id>_<event id>_c<content>_p<part><ext>` with the prefix
/// cleaned by [`clean_gcs_prefix`] (see [`gcs_object_path_from_clean_prefix`]).
#[allow(clippy::too_many_arguments)]
#[cfg_attr(not(test), allow(dead_code))] // the plugin calls the clean-prefix form
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
    gcs_object_path_from_clean_prefix(
        &clean_gcs_prefix(prefix),
        ts,
        trace_id,
        span_id,
        event_id,
        content_ord,
        part_idx,
        ext,
    )
}

/// The object name for an already-cleaned prefix. A name is returned as is only when it fits
/// [`MAX_GCS_OBJECT_NAME_BYTES`] and its extension is empty or [`is_safe_extension`]; a longer
/// name with a safe extension is cut before the extension and gets the `~<sha256>` suffix of
/// the whole name; a name with any other extension always ends in that hash (its readable part
/// with the extension sanitized).
#[allow(clippy::too_many_arguments)]
pub(crate) fn gcs_object_path_from_clean_prefix(
    clean_prefix: &str,
    ts: DateTime<Utc>,
    trace_id: &str,
    span_id: &str,
    event_id: &str,
    content_ord: usize,
    part_idx: usize,
    ext: &str,
) -> String {
    let mut out = String::new();
    if !clean_prefix.is_empty() {
        out.push_str(clean_prefix);
        out.push('/');
    }
    out.push_str(&format!(
        "{}/{}/{}_{}_c{content_ord}_p{part_idx}{ext}",
        ts.format("%Y-%m-%d"),
        path_segment(trace_id, MISSING_TRACE_ID),
        path_segment(span_id, MISSING_SPAN_ID),
        path_segment(event_id, MISSING_EVENT_ID),
    ));
    let safe_ext = is_safe_extension(ext);
    if out.len() <= MAX_GCS_OBJECT_NAME_BYTES && (ext.is_empty() || safe_ext) {
        return out;
    }
    let stem = &out[..out.len() - ext.len()];
    if safe_ext && ext.len() < MAX_GCS_OBJECT_NAME_BYTES - PATH_SEGMENT_HASH_SUFFIX_LEN {
        return truncate_with_hash(stem, &out, MAX_GCS_OBJECT_NAME_BYTES - ext.len()) + ext;
    }
    let readable = format!("{stem}{}", sanitize_extension(ext));
    truncate_with_hash(&readable, &out, MAX_GCS_OBJECT_NAME_BYTES)
}

/// Diagnostic metadata of `err` that never shows a custom offloader's error text: the context
/// errors and upload sentinels by name, an offloader panic's type, the built-in offloader's own
/// error scrubbed ([`redact_error_message`], then [`strip_uri_queries`]), a Google API error's
/// HTTP status (`HTTP <code>`), else `error type <name>` (see `go_error_type`).
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

/// The error type name reported in the summary (`error type <name>`), chosen by the [`Error`]
/// variant. The names are the ones an ADK server writes into these summaries, so they match
/// across implementations; the literals are in the match below. An `Error::Other` (a custom
/// offloader's own error type) or an Arrow error is `error`, as Rust has no runtime type name
/// for a `Box<dyn Error>`.
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

/// The URIs in `msg` without their query strings and fragments.
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
