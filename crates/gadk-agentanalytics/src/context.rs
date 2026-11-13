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
//! difference). Context values are not modeled.

use std::fmt;
use std::future::Future;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

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

struct Inner {
    parent: Option<Context>,
    /// The effective deadline: the earlier of this context's own and its parent's.
    deadline: Option<Instant>,
    /// Cancelled when this context ends (a child token of the parent's).
    token: CancellationToken,
    /// The first reason this context ended; never changes once set.
    err: OnceLock<ContextError>,
}

/// A Go `context.Context` (without values): a cheap `Clone` handle. A child made by
/// [`with_cancel`](Context::with_cancel) / [`with_timeout`](Context::with_timeout) /
/// [`with_deadline`](Context::with_deadline) ends when its parent ends (with the parent's
/// error), when [`cancel`](Context::cancel) is called on it (`Canceled`), or when its deadline
/// passes (`DeadlineExceeded`). Deadlines use [`tokio::time::Instant`], so tests may pause time.
#[derive(Clone)]
pub struct Context {
    inner: Arc<Inner>,
}

impl fmt::Debug for Context {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Context")
            .field("deadline", &self.inner.deadline)
            .field("err", &self.err())
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
                token: CancellationToken::new(),
                err: OnceLock::new(),
            }),
        }
    }

    fn child(&self, deadline: Option<Instant>) -> Context {
        let deadline = match (self.inner.deadline, deadline) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        Context {
            inner: Arc::new(Inner {
                parent: Some(self.clone()),
                deadline,
                token: self.inner.token.child_token(),
                err: OnceLock::new(),
            }),
        }
    }

    /// Go `context.WithCancel(ctx)`: a child that [`cancel`](Context::cancel) ends.
    pub fn with_cancel(&self) -> Context {
        self.child(None)
    }

    /// Go `context.WithTimeout(ctx, d)`.
    pub fn with_timeout(&self, d: Duration) -> Context {
        self.child(Instant::now().checked_add(d))
    }

    /// Go `context.WithDeadline(ctx, at)`.
    pub fn with_deadline(&self, at: Instant) -> Context {
        self.child(Some(at))
    }

    /// Go `context.WithoutCancel(ctx)`: a context that never ends (values are not modeled).
    pub fn without_cancel(&self) -> Context {
        Context::background()
    }

    /// Go `cancel()` of `WithCancel` / `WithTimeout` / `WithDeadline`: ends this context and its
    /// children with `Canceled`, unless it has already ended (then its error stays).
    pub fn cancel(&self) {
        if self.err().is_none() {
            let _ = self.inner.err.set(ContextError::Canceled);
        }
        self.inner.token.cancel();
    }

    /// Go `ctx.Deadline()`.
    pub fn deadline(&self) -> Option<Instant> {
        self.inner.deadline
    }

    /// Go `ctx.Err()`: `None` while the context is live.
    pub fn err(&self) -> Option<ContextError> {
        if let Some(err) = self.inner.err.get() {
            return Some(*err);
        }
        let ended = self
            .inner
            .parent
            .as_ref()
            .and_then(Context::err)
            .or_else(|| {
                self.inner
                    .deadline
                    .filter(|at| Instant::now() >= *at)
                    .map(|_| ContextError::DeadlineExceeded)
            })?;
        let _ = self.inner.err.set(ended);
        self.inner.token.cancel();
        self.inner.err.get().copied()
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
}
