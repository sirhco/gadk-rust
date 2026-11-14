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

//! [`Context`]: the subset of Go's `context.Context` that `plugin/agentanalytics` relies on —
//! cancellation, deadlines and `Err()` telling `context.Canceled` from
//! `context.DeadlineExceeded` (Go's `syncAnalyticsViews` and `viewDDLTimeout` depend on the
//! difference), timeout causes (`context.WithTimeoutCause` / `context.Cause`), `context.AfterFunc`,
//! and of Go's context values the one the transports carry: grpc-go's outgoing metadata
//! (`metadata.NewOutgoingContext` / `AppendToOutgoingContext` / `FromOutgoingContext`), which
//! every derived context — [`without_cancel`](Context::without_cancel) included — keeps.

use std::collections::BTreeMap;
use std::fmt;
use std::future::Future;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::Duration;

use tokio::task::AbortHandle;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::error::Error;

/// Why a [`Context`] ended (Go `context.Canceled` / `context.DeadlineExceeded`). `Display` is
/// Go's text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContextError {
    /// `context.Canceled`: `context canceled`.
    Canceled,
    /// `context.DeadlineExceeded`: `context deadline exceeded`.
    DeadlineExceeded,
}

impl fmt::Display for ContextError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ContextError::Canceled => "context canceled",
            ContextError::DeadlineExceeded => "context deadline exceeded",
        })
    }
}

impl std::error::Error for ContextError {}

/// grpc-go `metadata.MD`: lowercase keys, each with its values in order.
pub type Metadata = BTreeMap<String, Vec<String>>;

/// When a context ended, why, and its cause.
type Ended = (Instant, ContextError, Arc<Error>);

/// The function of [`Context::after_func`], taken by whoever starts it first.
type AfterFuncSlot = Arc<Mutex<Option<Box<dyn FnOnce() + Send>>>>;

struct Inner {
    parent: Option<Context>,
    /// The effective deadline: the earlier of this context's own and its parent's.
    deadline: Option<Instant>,
    /// This context's own deadline (Go's timer of `WithDeadline`), if it set one.
    own_deadline: Option<Instant>,
    /// Cancelled when this context ends (a child token of the parent's).
    token: CancellationToken,
    /// The cause recorded when this context's own deadline ends it (Go `WithTimeoutCause`);
    /// `None` records `DeadlineExceeded`.
    timeout_cause: Option<Arc<Error>>,
    /// The first reason this context ended and its cause (Go `context.Cause`), with the instant
    /// it ended at; never changes once set.
    ended: OnceLock<Ended>,
    /// grpc-go's outgoing metadata, copied into every derived context.
    outgoing: Option<Arc<Metadata>>,
}

/// A Go `context.Context`: a cheap `Clone` handle. A child made by
/// [`with_cancel`](Context::with_cancel) / [`with_timeout`](Context::with_timeout) /
/// [`with_timeout_cause`](Context::with_timeout_cause) /
/// [`with_deadline`](Context::with_deadline) ends when its parent ends (with the parent's
/// error and cause), when [`cancel`](Context::cancel) is called on it (`Canceled`), or when its
/// deadline passes (`DeadlineExceeded`). Deadlines use [`tokio::time::Instant`], so tests may
/// pause time. Of Go's context values only grpc-go's outgoing metadata is modeled
/// ([`with_outgoing_metadata`](Context::with_outgoing_metadata)).
#[derive(Clone)]
pub struct Context {
    inner: Arc<Inner>,
}

impl fmt::Debug for Context {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Context")
            .field("deadline", &self.inner.deadline)
            .field("err", &self.err())
            .field("outgoing_metadata", &self.inner.outgoing)
            .finish_non_exhaustive()
    }
}

impl Default for Context {
    fn default() -> Self {
        Context::background()
    }
}

impl Context {
    /// Go `context.Background()`: never ends.
    pub fn background() -> Context {
        Context {
            inner: Arc::new(Inner {
                parent: None,
                deadline: None,
                own_deadline: None,
                token: CancellationToken::new(),
                timeout_cause: None,
                ended: OnceLock::new(),
                outgoing: None,
            }),
        }
    }

    fn derive(
        &self,
        deadline: Option<Instant>,
        timeout_cause: Option<Arc<Error>>,
        outgoing: Option<Arc<Metadata>>,
    ) -> Context {
        let own_deadline = deadline;
        let deadline = match (self.inner.deadline, deadline) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        Context {
            inner: Arc::new(Inner {
                parent: Some(self.clone()),
                deadline,
                own_deadline,
                token: self.inner.token.child_token(),
                timeout_cause,
                ended: OnceLock::new(),
                outgoing,
            }),
        }
    }

    fn child(&self, deadline: Option<Instant>, timeout_cause: Option<Arc<Error>>) -> Context {
        self.derive(deadline, timeout_cause, self.inner.outgoing.clone())
    }

    /// Go `context.WithCancel(ctx)`: a child that [`cancel`](Context::cancel) ends.
    pub fn with_cancel(&self) -> Context {
        self.child(None, None)
    }

    /// Go `context.WithTimeout(ctx, d)`.
    pub fn with_timeout(&self, d: Duration) -> Context {
        self.child(Instant::now().checked_add(d), None)
    }

    /// Go `context.WithTimeoutCause(ctx, d, cause)`: as [`with_timeout`](Self::with_timeout),
    /// but when its own deadline ends it, [`cause`](Self::cause) is `cause` (its
    /// [`err`](Self::err) is still `DeadlineExceeded`). When the parent ends it first, the
    /// parent's cause is kept, as Go's `WithDeadlineCause` does.
    pub fn with_timeout_cause(&self, d: Duration, cause: Arc<Error>) -> Context {
        self.child(Instant::now().checked_add(d), Some(cause))
    }

    /// Go `context.WithDeadline(ctx, at)`.
    pub fn with_deadline(&self, at: Instant) -> Context {
        self.child(Some(at), None)
    }

    /// Go `context.WithoutCancel(ctx)`: a context that never ends but keeps `ctx`'s values
    /// (its outgoing metadata).
    pub fn without_cancel(&self) -> Context {
        Context {
            inner: Arc::new(Inner {
                parent: None,
                deadline: None,
                own_deadline: None,
                token: CancellationToken::new(),
                timeout_cause: None,
                ended: OnceLock::new(),
                outgoing: self.inner.outgoing.clone(),
            }),
        }
    }

    /// grpc-go `metadata.NewOutgoingContext(ctx, md)`: a child (ending with this context)
    /// whose outgoing metadata is `md` (keys lowercased), replacing any this context carries.
    pub fn with_outgoing_metadata(&self, md: Metadata) -> Context {
        let mut lower = Metadata::new();
        for (k, vals) in md {
            lower.entry(k.to_lowercase()).or_default().extend(vals);
        }
        self.derive(None, None, Some(Arc::new(lower)))
    }

    /// grpc-go `metadata.AppendToOutgoingContext(ctx, kv...)`: a child (ending with this
    /// context) whose outgoing metadata is this context's with `pairs` appended (keys
    /// lowercased).
    pub fn append_outgoing_metadata(&self, pairs: &[(&str, &str)]) -> Context {
        let mut md = self.outgoing_metadata().unwrap_or_default();
        for (k, v) in pairs {
            md.entry(k.to_lowercase())
                .or_default()
                .push((*v).to_owned());
        }
        self.derive(None, None, Some(Arc::new(md)))
    }

    /// grpc-go `metadata.FromOutgoingContext(ctx)`: a copy of the outgoing metadata, `None`
    /// when no context in the chain set any.
    pub fn outgoing_metadata(&self) -> Option<Metadata> {
        self.inner.outgoing.as_deref().cloned()
    }

    /// Go `cancel()` of `WithCancel` / `WithTimeout` / `WithDeadline`: ends this context and its
    /// children with `Canceled`, unless it has already ended (then its error stays).
    pub fn cancel(&self) {
        if self.ended().is_none() {
            let _ = self.inner.ended.set((
                Instant::now(),
                ContextError::Canceled,
                Arc::new(Error::Context(ContextError::Canceled)),
            ));
        }
        self.inner.token.cancel();
    }

    /// Go `ctx.Deadline()`.
    pub fn deadline(&self) -> Option<Instant> {
        self.inner.deadline
    }

    /// Why this context ended, if it has, latched as of the instant it ended (Go fixes `Err` and
    /// `Cause` when the timer fires or the cancel happens): the earliest of the parent's end, this
    /// context's own deadline, and its own cancel (which [`cancel`](Self::cancel) latches at
    /// once). A deadline is the instant it passed, so a context whose own timeout passed before a
    /// later end of its parent keeps its own cause however late it is first asked; on a tie the
    /// parent's end wins (its deadline is the child's effective one).
    fn ended(&self) -> Option<&Ended> {
        if let Some(ended) = self.inner.ended.get() {
            return Some(ended);
        }
        let parent = self.inner.parent.as_ref().and_then(Context::ended);
        let own = self
            .inner
            .own_deadline
            .filter(|at| Instant::now() >= *at)
            .filter(|at| parent.is_none_or(|(ended_at, _, _)| at < ended_at));
        let ended = match (parent, own) {
            (_, Some(at)) => {
                let cause =
                    self.inner.timeout_cause.clone().unwrap_or_else(|| {
                        Arc::new(Error::Context(ContextError::DeadlineExceeded))
                    });
                (at, ContextError::DeadlineExceeded, cause)
            }
            (Some((at, err, cause)), None) => (*at, *err, Arc::clone(cause)),
            (None, None) => return None,
        };
        let _ = self.inner.ended.set(ended);
        self.inner.token.cancel();
        self.inner.ended.get()
    }

    /// Go `ctx.Err()`: `None` while the context is live.
    pub fn err(&self) -> Option<ContextError> {
        self.ended().map(|(_, err, _)| *err)
    }

    /// Go `context.Cause(ctx)`: `None` while the context is live; once it ended, the cause given
    /// to [`with_timeout_cause`](Self::with_timeout_cause) when that deadline ended it, else
    /// the parent's cause when the parent ended it, else [`Error::Context`] of
    /// [`err`](Self::err). The same `Arc` is returned every time (compare with
    /// [`Arc::ptr_eq`]).
    pub fn cause(&self) -> Option<Arc<Error>> {
        self.ended().map(|(_, _, cause)| Arc::clone(cause))
    }

    /// Go `<-ctx.Done()`: resolves once the context has ended.
    pub async fn done(&self) {
        match self.inner.deadline {
            Some(at) => {
                tokio::select! {
                    () = self.inner.token.cancelled() => {}
                    () = tokio::time::sleep_until(at) => {}
                }
            }
            None => self.inner.token.cancelled().await,
        }
    }

    /// Go `context.AfterFunc(ctx, f)`: runs `f` once, on a spawned task, after this context
    /// ends (at once if it already has), unless [`AfterFunc::stop`] is called first. Dropping
    /// the handle does not stop it. Must be called inside a Tokio runtime.
    pub fn after_func(&self, f: impl FnOnce() + Send + 'static) -> AfterFunc {
        let f: Box<dyn FnOnce() + Send> = Box::new(f);
        let slot: AfterFuncSlot = Arc::new(Mutex::new(Some(f)));
        let ctx = self.clone();
        let task_slot = Arc::clone(&slot);
        let task = tokio::spawn(async move {
            ctx.done().await;
            // Record the end, as Go's cancellation does before it starts `f`.
            let _ = ctx.err();
            let f = task_slot
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take();
            if let Some(f) = f {
                f();
            }
        });
        AfterFunc {
            ctx: self.clone(),
            slot,
            task: task.abort_handle(),
        }
    }

    /// Runs `fut` until it finishes or the context ends; an ended context (also one that had
    /// already ended) returns [`Error::Context`] with [`err`](Context::err) and drops `fut`
    /// (Go's request cancellation).
    pub async fn run<T, F>(&self, fut: F) -> Result<T, Error>
    where
        F: Future<Output = Result<T, Error>>,
    {
        if let Some(err) = self.err() {
            return Err(Error::Context(err));
        }
        tokio::select! {
            biased;
            () = self.done() => Err(Error::Context(self.err().unwrap_or(ContextError::Canceled))),
            res = fut => res,
        }
    }
}

/// The handle of [`Context::after_func`] (Go's `stop` function).
pub struct AfterFunc {
    ctx: Context,
    slot: AfterFuncSlot,
    task: AbortHandle,
}

impl fmt::Debug for AfterFunc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AfterFunc").finish_non_exhaustive()
    }
}

impl AfterFunc {
    /// Go `stop()`: `true` if it kept `f` from running, `false` if `f` has started (or the
    /// context has ended, which starts it — here on the calling thread when the watcher task
    /// has not taken it yet) or `stop` was already called. It does not wait for `f`.
    ///
    /// Because `f` may run inline here, `f` must not block or take locks the caller holds.
    pub fn stop(&self) -> bool {
        let take = || {
            self.slot
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take()
        };
        if self.ctx.err().is_some() {
            if let Some(f) = take() {
                f();
            }
            return false;
        }
        match take() {
            Some(_) => {
                self.task.abort();
                true
            }
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn background_never_ends() {
        let ctx = Context::background();
        assert_eq!(ctx.err(), None);
        assert_eq!(ctx.deadline(), None);
    }

    #[test]
    fn cancel_ends_the_child_and_its_children_only() {
        let parent = Context::background();
        let child = parent.with_cancel();
        let grandchild = child.with_cancel();
        child.cancel();
        assert_eq!(child.err(), Some(ContextError::Canceled));
        assert_eq!(grandchild.err(), Some(ContextError::Canceled));
        assert_eq!(parent.err(), None);
        // A second cancel keeps the first reason.
        child.cancel();
        assert_eq!(child.err(), Some(ContextError::Canceled));
    }

    #[tokio::test(start_paused = true)]
    async fn timeout_ends_with_deadline_exceeded_and_cancel_keeps_it() {
        let ctx = Context::background().with_timeout(Duration::from_millis(100));
        assert_eq!(ctx.err(), None);
        ctx.done().await;
        assert_eq!(ctx.err(), Some(ContextError::DeadlineExceeded));
        ctx.cancel();
        assert_eq!(ctx.err(), Some(ContextError::DeadlineExceeded));
    }

    #[tokio::test(start_paused = true)]
    async fn child_takes_the_earlier_deadline_and_the_parents_error() {
        let parent = Context::background().with_timeout(Duration::from_millis(50));
        let child = parent.with_timeout(Duration::from_secs(30));
        assert_eq!(child.deadline(), parent.deadline());
        let canceled = Context::background().with_cancel();
        let under = canceled.with_timeout(Duration::from_secs(30));
        canceled.cancel();
        assert_eq!(under.err(), Some(ContextError::Canceled));
        child.done().await;
        assert_eq!(child.err(), Some(ContextError::DeadlineExceeded));
    }

    #[tokio::test(start_paused = true)]
    async fn run_returns_the_context_error_or_the_result() {
        let ctx = Context::background().with_timeout(Duration::from_millis(10));
        let ok: Result<u8, Error> = ctx.run(async { Ok(7) }).await;
        assert_eq!(ok.unwrap(), 7);
        let err = ctx
            .run(async {
                tokio::time::sleep(Duration::from_secs(1)).await;
                Ok::<_, Error>(())
            })
            .await
            .unwrap_err();
        assert!(err.is_context(ContextError::DeadlineExceeded), "{err}");
        let canceled = Context::background().with_cancel();
        canceled.cancel();
        let err = canceled
            .run(async { Ok::<_, Error>(()) })
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "context canceled");
    }

    #[test]
    fn without_cancel_is_detached() {
        let ctx = Context::background().with_cancel();
        let detached = ctx.without_cancel();
        ctx.cancel();
        assert_eq!(detached.err(), None);
    }

    fn md(pairs: &[(&str, &[&str])]) -> Metadata {
        pairs
            .iter()
            .map(|(k, vs)| {
                (
                    (*k).to_owned(),
                    vs.iter().map(|v| (*v).to_owned()).collect(),
                )
            })
            .collect()
    }

    #[tokio::test(start_paused = true)]
    async fn outgoing_metadata_survives_every_derivation() {
        let base = Context::background();
        assert_eq!(base.outgoing_metadata(), None);
        let ctx = base
            .with_cancel()
            .with_outgoing_metadata(md(&[("X-Test-Caller", &["preserve-me"])]));
        let want = md(&[("x-test-caller", &["preserve-me"])]);
        assert_eq!(ctx.outgoing_metadata(), Some(want.clone()));
        for derived in [
            ctx.without_cancel(),
            ctx.with_cancel(),
            ctx.with_timeout(Duration::from_secs(1)),
            ctx.with_deadline(Instant::now()),
            ctx.with_timeout_cause(Duration::from_secs(1), Arc::new(Error::msg("c"))),
            ctx.without_cancel().with_cancel().without_cancel(),
        ] {
            assert_eq!(derived.outgoing_metadata(), Some(want.clone()));
        }
        // The value context ends with its parent.
        let parent = Context::background().with_cancel();
        let valued = parent.with_outgoing_metadata(Metadata::new());
        parent.cancel();
        assert_eq!(valued.err(), Some(ContextError::Canceled));
    }

    #[test]
    fn append_and_new_outgoing_metadata_follow_grpc_go() {
        let ctx = Context::background()
            .append_outgoing_metadata(&[("X-Goog-Request-Params", "a=1")])
            .append_outgoing_metadata(&[("x-goog-request-params", "b=2"), ("k", "v")]);
        assert_eq!(
            ctx.outgoing_metadata(),
            Some(md(&[
                ("k", &["v"]),
                ("x-goog-request-params", &["a=1", "b=2"])
            ]))
        );
        // NewOutgoingContext replaces what the parent carried.
        let replaced = ctx.with_outgoing_metadata(md(&[("only", &["1"])]));
        assert_eq!(replaced.outgoing_metadata(), Some(md(&[("only", &["1"])])));
        assert_eq!(
            ctx.without_cancel()
                .append_outgoing_metadata(&[("k", "w")])
                .outgoing_metadata()
                .unwrap()["k"],
            ["v", "w"]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn cause_tells_the_own_timeout_from_the_parents_deadline_and_cancel() {
        let cause = Arc::new(Error::msg(
            "batch upload timeout: context deadline exceeded",
        ));
        // Live: no cause.
        let ctx =
            Context::background().with_timeout_cause(Duration::from_millis(10), cause.clone());
        assert!(ctx.cause().is_none());
        // Own timeout: the given cause, Err is DeadlineExceeded.
        ctx.done().await;
        assert_eq!(ctx.err(), Some(ContextError::DeadlineExceeded));
        assert!(Arc::ptr_eq(&ctx.cause().unwrap(), &cause));
        // The parent's earlier deadline: the parent's cause.
        let parent = Context::background().with_timeout(Duration::from_millis(5));
        let child = parent.with_timeout_cause(Duration::from_secs(60), cause.clone());
        child.done().await;
        assert_eq!(child.err(), Some(ContextError::DeadlineExceeded));
        let got = child.cause().unwrap();
        assert!(!Arc::ptr_eq(&got, &cause));
        assert!(got.is_context(ContextError::DeadlineExceeded));
        assert!(Arc::ptr_eq(&got, &parent.cause().unwrap()));
        // Canceled before the deadline: Canceled.
        let ctx = Context::background().with_timeout_cause(Duration::from_secs(60), cause.clone());
        ctx.cancel();
        assert!(ctx.cause().unwrap().is_context(ContextError::Canceled));
        // A canceled parent: the child's cause is the parent's.
        let parent = Context::background().with_cancel();
        let child = parent.with_timeout_cause(Duration::from_secs(60), cause.clone());
        parent.cancel();
        assert!(Arc::ptr_eq(
            &child.cause().unwrap(),
            &parent.cause().unwrap()
        ));
        // Plain timeouts have DeadlineExceeded as their cause; the cause never changes.
        let plain = Context::background().with_timeout(Duration::from_millis(1));
        plain.done().await;
        let first = plain.cause().unwrap();
        assert_eq!(first.to_string(), "context deadline exceeded");
        plain.cancel();
        assert!(Arc::ptr_eq(&first, &plain.cause().unwrap()));
    }

    #[tokio::test(start_paused = true)]
    async fn the_end_is_latched_at_the_instant_it_happened_not_when_first_asked() {
        let cause = Arc::new(Error::msg(
            "batch upload timeout: context deadline exceeded",
        ));
        // The child's own timeout (10ms) passes before the parent's deadline (50ms); nobody asks
        // until both passed: the child reports its own cause, the parent its own.
        let parent = Context::background().with_timeout(Duration::from_millis(50));
        let child = parent.with_timeout_cause(Duration::from_millis(10), cause.clone());
        tokio::time::sleep(Duration::from_millis(100)).await;
        let parent_cause = parent.cause().unwrap();
        assert!(parent_cause.is_context(ContextError::DeadlineExceeded));
        assert_eq!(child.err(), Some(ContextError::DeadlineExceeded));
        assert!(Arc::ptr_eq(&child.cause().unwrap(), &cause));
        // Same when the parent is canceled after the child's timeout passed unobserved.
        let parent = Context::background().with_cancel();
        let child = parent.with_timeout_cause(Duration::from_millis(10), cause.clone());
        tokio::time::advance(Duration::from_millis(20)).await;
        parent.cancel();
        assert_eq!(child.err(), Some(ContextError::DeadlineExceeded));
        assert!(Arc::ptr_eq(&child.cause().unwrap(), &cause));
        // A grandchild of the child ends with the child's cause too.
        let parent = Context::background().with_timeout(Duration::from_millis(50));
        let child = parent.with_timeout_cause(Duration::from_millis(10), cause.clone());
        let grandchild = child.with_cancel();
        tokio::time::advance(Duration::from_millis(60)).await;
        assert!(Arc::ptr_eq(&grandchild.cause().unwrap(), &cause));
        // The parent canceled before the child's own timeout: the parent's cause, even when
        // first asked after the child's deadline passed.
        let parent = Context::background().with_cancel();
        let child = parent.with_timeout_cause(Duration::from_millis(30), cause.clone());
        tokio::time::advance(Duration::from_millis(5)).await;
        parent.cancel();
        tokio::time::advance(Duration::from_millis(60)).await;
        assert_eq!(child.err(), Some(ContextError::Canceled));
        assert!(Arc::ptr_eq(
            &child.cause().unwrap(),
            &parent.cause().unwrap()
        ));
        // The child's own cancel after its timeout passed unobserved keeps the timeout.
        let ctx =
            Context::background().with_timeout_cause(Duration::from_millis(10), cause.clone());
        tokio::time::advance(Duration::from_millis(20)).await;
        ctx.cancel();
        assert!(Arc::ptr_eq(&ctx.cause().unwrap(), &cause));
    }

    fn counter() -> (
        Arc<std::sync::atomic::AtomicUsize>,
        impl FnOnce() + Send + 'static,
    ) {
        let n = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let m = Arc::clone(&n);
        (n, move || {
            m.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        })
    }

    fn runs(n: &std::sync::atomic::AtomicUsize) -> usize {
        n.load(std::sync::atomic::Ordering::SeqCst)
    }

    #[tokio::test(start_paused = true)]
    async fn after_func_runs_once_when_the_context_ends() {
        let ctx = Context::background().with_cancel();
        let (n, f) = counter();
        let stop = ctx.after_func(f);
        tokio::task::yield_now().await;
        assert_eq!(runs(&n), 0);
        ctx.cancel();
        tokio::time::sleep(Duration::from_millis(1)).await;
        assert_eq!(runs(&n), 1);
        // f has started: stop reports false and nothing runs again.
        assert!(!stop.stop());
        assert!(!stop.stop());
        assert_eq!(runs(&n), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn after_func_stop_before_the_end_keeps_f_from_running() {
        let ctx = Context::background().with_timeout(Duration::from_millis(10));
        let (n, f) = counter();
        let stop = ctx.after_func(f);
        assert!(stop.stop());
        assert!(!stop.stop());
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(ctx.err(), Some(ContextError::DeadlineExceeded));
        assert_eq!(runs(&n), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn after_func_on_an_ended_context_runs_and_stop_reports_false() {
        let ctx = Context::background().with_cancel();
        ctx.cancel();
        let (n, f) = counter();
        let stop = ctx.after_func(f);
        // Whether the watcher or stop starts f, it runs exactly once and stop says false.
        assert!(!stop.stop());
        tokio::time::sleep(Duration::from_millis(1)).await;
        assert_eq!(runs(&n), 1);
        // A deadline that passed unobserved counts as ended too.
        let ctx = Context::background().with_timeout(Duration::from_millis(5));
        let (n, f) = counter();
        let stop = ctx.after_func(f);
        tokio::time::advance(Duration::from_millis(10)).await;
        assert!(!stop.stop());
        tokio::time::sleep(Duration::from_millis(1)).await;
        assert_eq!(runs(&n), 1);
    }
}
