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

//! Port of the compaction parts of Go runner/runner.go: config resolution
//! (`resolveCompactionConfig`), post-invocation sliding-window compaction
//! (`compactAfterInvocation`), and the deferred calls of `Run`/`runNode` as one guard.

use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Duration;

use futures::FutureExt;
use gadk_agent::fiber::{self, Fiber};
use gadk_agent::{Agent, AgentError, ContextDelta, InvocationContext, go_quote};
use gadk_session::compaction::{CompactionError, Config, LlmSummarizer, LlmSummarizerConfig};
use gadk_session::{Event, Session, SessionService};
use tokio_util::sync::{CancellationToken, DropGuard};
use tracing::Instrument;

use super::RunnerInner;
use crate::compaction::{BoxError, apply, compactor, has_sliding_window, runtime, summary_event};
use crate::llm::state::reveal;
use crate::tool::utils::panic_message;

/// Go `defaultSummarizerTimeout`: the bound on the summarizer the runner installs itself.
pub(crate) const DEFAULT_SUMMARIZER_TIMEOUT: Duration = Duration::from_secs(60);

/// Port of Go resolveCompactionConfig: validates `cfg` and fills in the default summarizer
/// (an [`LlmSummarizer`] over the root agent's model and generation config). The caller's
/// config is never mutated; `None` means compaction is disabled.
pub(crate) fn resolve_compaction_config(
    cfg: Option<&Config>,
    root: &Arc<dyn Agent>,
    summarizer_timeout: Duration,
) -> Result<Option<Arc<Config>>, AgentError> {
    let Some(cfg) = cfg else {
        return Ok(None);
    };
    cfg.validate()
        .map_err(|e| AgentError::Config(format!("invalid Compaction: {e}")))?;
    // Copy so the caller's config is not mutated by the summarizer default.
    let mut resolved = cfg.clone();
    if resolved.summarizer.is_some() {
        return Ok(Some(Arc::new(resolved)));
    }
    let Some(state) = reveal(&**root) else {
        return Err(AgentError::Config(format!(
            "the Compaction config needs a Summarizer: root agent {} is not an LLM agent, so no default model is available",
            go_quote(root.name())
        )));
    };
    let Some(model) = state.cfg.model.clone() else {
        return Err(AgentError::Config(format!(
            "the Compaction config needs a Summarizer: root agent {} has no model",
            go_quote(root.name())
        )));
    };
    let summarizer = LlmSummarizer::new(LlmSummarizerConfig {
        model: Some(model),
        generate_content_config: state.cfg.generate_content_config.clone(),
        timeout: summarizer_timeout,
        ..Default::default()
    })
    .map_err(|e| {
        AgentError::Config(format!(
            "failed to create the default compaction summarizer: {e}"
        ))
    })?;
    resolved.summarizer = Some(Arc::new(summarizer));
    Ok(Some(Arc::new(resolved)))
}

/// Go `fmt.Errorf("%w: post-invocation: %w", compaction.ErrCompaction, err)`.
fn post_invocation(err: BoxError) -> AgentError {
    AgentError::Compaction(CompactionError::wrap("post-invocation", err))
}

/// Go compactAfterInvocation's `discardRaced`.
fn discard_raced(finish: compactor::Finish) {
    finish.finish(
        None,
        "another compaction covering the same events landed while summarizing",
    );
    tracing::warn!(
        "adk: discarding a context compaction summary because the session changed inside its range while summarizing"
    );
}

impl RunnerInner {
    /// Port of Go (*Runner).compactAfterInvocation: sliding-window compaction once an
    /// invocation's events are persisted. The summary is stored, not yielded. A panic anywhere
    /// inside (a summarizer's, resumed by the compactor's span) becomes a compaction error
    /// rather than unwinding into the caller.
    pub(crate) async fn compact_after_invocation(
        &self,
        stored: &Arc<dyn Session>,
        ictx: &InvocationContext,
    ) -> Result<(), AgentError> {
        let Some(cfg) = self.compaction.clone() else {
            return Ok(());
        };
        if !has_sliding_window(Some(&cfg)) {
            return Ok(());
        }
        match AssertUnwindSafe(self.compact_after_invocation_inner(&cfg, stored, ictx))
            .catch_unwind()
            .await
        {
            Ok(r) => r,
            Err(payload) => Err(AgentError::Compaction(CompactionError::new(format!(
                "post-invocation: compaction panicked: {}",
                panic_message(&*payload)
            )))),
        }
    }

    async fn compact_after_invocation_inner(
        &self,
        cfg: &Config,
        stored: &Arc<dyn Session>,
        ictx: &InvocationContext,
    ) -> Result<(), AgentError> {
        // Tail retention already compacted this invocation: do not summarize it again.
        if runtime::already_compacted(ictx) {
            return Ok(());
        }
        // Compaction is an optimisation, so a cancelled run does not start one.
        if ictx.cancellation_token().is_cancelled() {
            return Ok(());
        }

        // Re-read: the handle the invocation began with predates the turn.
        let current = apply::reload_session(Some(&*self.session_service), &**stored)
            .await
            .map_err(post_invocation)?;
        let (summary, finish) =
            compactor::sliding_window(ictx.platform(), cfg, Some(&*current), ictx.invocation_id())
                .await
                .map_err(post_invocation)?;
        let Some(mut summary) = summary else {
            return Ok(());
        };

        if ictx.cancellation_token().is_cancelled() {
            finish.finish(None, "the run ended before the summary could be stored");
            return Ok(());
        }
        // The identities present when the window was chosen, for the repair after the append.
        let known = apply::known_event_ids(&*current);

        match self.summary_raced(stored, &*current, &summary).await {
            Err(e) => {
                finish.finish(Some(&*e), "");
                return Err(post_invocation(e));
            }
            Ok(true) => {
                discard_raced(finish);
                return Ok(());
            }
            Ok(false) => {}
        }

        // Plugins see the summary before it is stored. Go takes a plugin's replacement as is
        // (no fromPlugin: a plugin may rewrite a summary) and filters it like a summarizer's
        // output.
        match self
            .plugin_manager
            .run_on_event(ictx.clone(), summary.clone())
            .await
        {
            Err(e) => {
                finish.finish(Some(&e), "");
                return Err(AgentError::Compaction(CompactionError::wrap(
                    "plugin rejected the summary event",
                    e,
                )));
            }
            Ok(Some(mut modified)) => {
                if !summary_event::sanitize_summary(&mut modified) {
                    finish.finish(None, "a plugin left the summary with nothing usable in it");
                    tracing::warn!(
                        "adk: discarding a context compaction summary because a plugin left no usable content in it"
                    );
                    return Ok(());
                }
                summary = modified;
            }
            Ok(None) => {}
        }

        // The plugin pass is the widest part of the window, so the check is repeated.
        match self.summary_raced(stored, &*current, &summary).await {
            Err(e) => {
                finish.finish(Some(&*e), "");
                return Err(post_invocation(e));
            }
            Ok(true) => {
                discard_raced(finish);
                return Ok(());
            }
            Ok(false) => {}
        }

        if let Err(e) = self
            .session_service
            .append_event(&*current, &mut summary)
            .await
        {
            finish.finish(Some(&e), "");
            return Err(AgentError::Compaction(CompactionError::wrap(
                "failed to append the summary event",
                e,
            )));
        }
        finish.finish(None, "");

        // Go RepairContext: what lands during the append is repaired after the fact, detached
        // from the run's cancellation (spawned, so dropping this future does not stop it) and
        // bounded by the repair timeout; failures are logged.
        let repaired = apply::repair(repair_after_append(
            Arc::clone(&self.session_service),
            Arc::clone(stored),
            current,
            summary,
            known,
        ))
        .await;
        if repaired.is_none() {
            tracing::warn!(
                "adk: could not check a stored compaction for stragglers: the repair timed out"
            );
        }
        Ok(())
    }

    /// Go compactAfterInvocation's `raced`: re-reads the session and reports whether anything
    /// landed inside the summary's range since the window was chosen.
    async fn summary_raced(
        &self,
        stored: &Arc<dyn Session>,
        current: &dyn Session,
        summary: &Event,
    ) -> Result<bool, BoxError> {
        let latest = apply::reload_session(Some(&*self.session_service), &**stored).await?;
        Ok(apply::range_raced(&*latest, current, summary))
    }
}

/// Go compactAfterInvocation's straggler repair: stores a corrected record naming every event
/// that landed inside the stored summary's range unseen. Owns its inputs so [`apply::repair`]
/// can detach it from the run.
async fn repair_after_append(
    svc: Arc<dyn SessionService>,
    stored: Arc<dyn Session>,
    current: Arc<dyn Session>,
    summary: Event,
    known: std::collections::HashSet<String>,
) {
    let latest = match apply::reload_session(Some(&*svc), &*stored).await {
        Ok(latest) => latest,
        Err(e) => {
            tracing::warn!(
                "adk: could not re-read the session to check a stored compaction for stragglers: {e}"
            );
            return;
        }
    };
    let Some(mut repair) = apply::repair_after_append(&summary, &known, &*latest) else {
        return;
    };
    let excluded = |ev: &Event| {
        ev.actions
            .compaction
            .as_ref()
            .map_or(0, |c| c.excluded_events.len())
    };
    let added = excluded(&repair).saturating_sub(excluded(&summary));
    match svc.append_event(&*current, &mut repair).await {
        Err(e) => tracing::warn!("adk: could not store a corrected compaction record: {e}"),
        Ok(()) => tracing::warn!(
            "adk: corrected a compaction record that would have covered {added} event(s) it did not summarize"
        ),
    }
}

/// Go's two deferred calls in `Run`/`runNode` as one guard: `compactOnce` (registered first,
/// so it runs last on an early return) and `RunAfterRunCallback` (registered by
/// [`InvocationGuard::defer_after_run`], where Go registers it).
///
/// - Normal end: [`InvocationGuard::compact_inline`] (its error is the stream's last item),
///   then [`InvocationGuard::finish`] (after-run callbacks).
/// - Early return inside the stream: [`InvocationGuard::finish_early`]: after-run, then
///   compaction with its error logged.
/// - Stream dropped: `Drop` spawns one task on the current runtime that runs the after-run
///   callbacks and then, if the stream was dropped at a yield, compaction on a context
///   re-derived with a fresh cancellation token (Go's `break` leaves the caller's context
///   uncancelled), inside the span and the fiber that were current when the guard was made
///   (Go runs the deferred calls on the consumer's goroutine; the guard is made inside the
///   runner's consumer `FiberStream`). Errors are logged.
///
/// Before the after-run callbacks (inline or spawned) the guard awaits the invocation's teardown
/// registry ([`gadk_agent::Teardowns`]): the started work that components dropped during the run
/// handed over (a ParallelAgent's branches, a workflow scheduler's node tasks, in-flight tool
/// calls), which in Go finishes on the consumer's goroutine before the deferred
/// `RunAfterRunCallback`. The token is still live while it is awaited after a stop at a yield.
///
/// The guard also owns the invocation token's cancel-on-drop ([`InvocationGuard::cancel_on_drop`]).
/// A drop inside an await cancels it at once (Go's caller cancelled its context, and the
/// after-run callbacks see that). A drop at a yield (Go's `break`, after which the deferred
/// `RunAfterRunCallback` runs on the caller's still-live context) cancels it only once the
/// spawned after-run callbacks return, so they, and every span record holding the token, see
/// a live context, as in Go.
///
/// Compaction is skipped once the invocation yielded an error ([`InvocationGuard::mark_failed`]),
/// and when the stream is dropped while suspended inside an await rather than at a yield
/// ([`InvocationGuard::at_yield`]).
///
/// A drop before the summary append is Go's cancellation decline; once the summary is stored,
/// the straggler repair is detached ([`apply::repair`] spawns it) and always runs.
///
/// The live path ([`InvocationGuard::no_compaction`]) uses only the after-run and
/// cancel-on-drop parts: Go `RunLive` does not compact.
pub(crate) struct InvocationGuard {
    inner: Arc<RunnerInner>,
    stored: Arc<dyn Session>,
    ctx: InvocationContext,
    failed: bool,
    /// The stream is suspended at a `yield` of an event (see [`InvocationGuard::at_yield`]).
    yielding: bool,
    after_run_pending: bool,
    compaction_pending: bool,
    /// Cancels the invocation token when dropped (see [`InvocationGuard::cancel_on_drop`]).
    cancel_on_drop: Option<DropGuard>,
    span: tracing::Span,
    /// The consumer's fiber, re-entered by the task `Drop` spawns.
    fiber: Fiber,
}

impl InvocationGuard {
    /// Go's `defer compactOnce()`; the after-run callbacks are not pending yet.
    pub(crate) fn new(
        inner: Arc<RunnerInner>,
        stored: Arc<dyn Session>,
        ctx: InvocationContext,
    ) -> Self {
        Self {
            inner,
            stored,
            ctx,
            failed: false,
            yielding: false,
            after_run_pending: false,
            compaction_pending: true,
            cancel_on_drop: None,
            span: tracing::Span::current(),
            fiber: fiber::current(),
        }
    }

    /// No compaction for this invocation (Go `RunLive`, which attaches no compaction runtime).
    pub(crate) fn no_compaction(&mut self) {
        self.compaction_pending = false;
    }

    /// Hands the invocation token's cancel-on-drop to the guard, which cancels it when the
    /// guard is dropped, after the after-run callbacks when the stream was dropped at a yield.
    pub(crate) fn cancel_on_drop(&mut self, guard: DropGuard) {
        self.cancel_on_drop = Some(guard);
    }

    /// Go's `defer pluginManager.RunAfterRunCallback(ctx)`.
    pub(crate) fn defer_after_run(&mut self) {
        self.after_run_pending = true;
    }

    /// Go's yield wrapper: call immediately before yielding any error.
    pub(crate) fn mark_failed(&mut self) {
        self.failed = true;
    }

    /// Brackets every `yield` (of an event or an error): `true` immediately before it, `false`
    /// immediately after the stream resumes. A consumer can only stop early at a yield (Go's
    /// `break`, after which the deferred after-run callbacks and compaction run on the caller's
    /// still-live context; an error yield has already skipped compaction). A stream dropped
    /// anywhere else was dropped inside an await (a pending model call or append), which is
    /// Go's cancelled context: the model call returns `ctx.Err()` and sets `invocationFailed`,
    /// or `compactAfterInvocation`'s `ctx.Err() != nil` check (runner.go) declines. So `Drop`
    /// compacts only when this is set.
    pub(crate) fn at_yield(&mut self, yielding: bool) {
        self.yielding = yielding;
    }

    /// Go's explicit `compactOnce()` after the event loop.
    pub(crate) async fn compact_inline(&mut self) -> Option<AgentError> {
        if !std::mem::take(&mut self.compaction_pending) || self.failed {
            return None;
        }
        self.inner
            .compact_after_invocation(&self.stored, &self.ctx)
            .await
            .err()
    }

    /// Go's deferred `RunAfterRunCallback`, at the end of a normal run.
    pub(crate) async fn finish(mut self) {
        if std::mem::take(&mut self.after_run_pending) {
            wait_teardowns(&self.ctx).await;
            self.inner
                .plugin_manager
                .run_after_run(self.ctx.clone())
                .await;
        }
    }

    /// An early `return`: Go's defers in LIFO order, after-run, then compaction (logged).
    pub(crate) async fn finish_early(mut self) {
        if std::mem::take(&mut self.after_run_pending) {
            wait_teardowns(&self.ctx).await;
            self.inner
                .plugin_manager
                .run_after_run(self.ctx.clone())
                .await;
        }
        if let Some(e) = self.compact_inline().await {
            tracing::warn!("adk: {e}");
        }
    }
}

/// Awaits the started work that components dropped during the invocation handed to its
/// teardown registry ([`gadk_agent::Teardowns`]): in Go that work finishes on the consumer's
/// goroutine before the deferred `RunAfterRunCallback` runs.
async fn wait_teardowns(ctx: &InvocationContext) {
    if let Some(t) = ctx.teardowns() {
        t.wait().await;
    }
}

// Divergence (ledger 7b-3a): a consumer that stops early unwinds Go's iterator on its own
// goroutine (teardown, `RunAfterRunCallback`, compaction) before its `break` returns; a dropped
// Rust stream cannot block its dropper, so this runs that work on a spawned task re-entering the
// consumer's fiber, which may overlap whatever the consumer does next.
impl Drop for InvocationGuard {
    fn drop(&mut self) {
        let after_run = std::mem::take(&mut self.after_run_pending);
        // Dropped mid-await is cancellation, not an early stop (see `at_yield`).
        let pending = std::mem::take(&mut self.compaction_pending) && !self.failed;
        if pending && !self.yielding {
            tracing::debug!("adk: skipping context compaction: the run was dropped mid-await");
        }
        let compact = pending && self.yielding;
        if !after_run && !compact {
            return;
        }
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        // At a yield the token outlives the after-run callbacks (the task drops the guard after
        // them, or when it is itself dropped unrun); inside an await the field's drop (after
        // this body) cancels it before the spawned task runs.
        let cancel_later = if self.yielding {
            self.cancel_on_drop.take()
        } else {
            None
        };
        let (inner, stored, ctx) = (self.inner.clone(), self.stored.clone(), self.ctx.clone());
        let task = self.fiber.clone().scope(async move {
            if after_run {
                wait_teardowns(&ctx).await;
                inner.plugin_manager.run_after_run(ctx.clone()).await;
            }
            drop(cancel_later);
            if compact {
                let fresh = ctx.with_delta(ContextDelta {
                    cancellation_token: Some(CancellationToken::new()),
                    ..Default::default()
                });
                if let Err(e) = inner.compact_after_invocation(&stored, &fresh).await {
                    tracing::warn!("adk: {e}");
                }
            }
        });
        handle.spawn(task.instrument(self.span.clone()));
    }
}
