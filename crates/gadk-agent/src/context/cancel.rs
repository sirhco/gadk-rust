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

//! Cancel causes on gadk contexts: why a context was cancelled, as used by
//! `Context::with_agent_cancel` and `Context::with_agent_timeout`.

use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::error::AgentError;

/// Why a context was cancelled.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub enum CancelCause {
    /// A plain cancel.
    Canceled,
    /// A timeout fired.
    DeadlineExceeded,
    /// Cancelled with an error: the shared error value.
    Error(Arc<dyn std::error::Error + Send + Sync>),
}

impl std::fmt::Display for CancelCause {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CancelCause::Canceled => f.write_str("context canceled"),
            CancelCause::DeadlineExceeded => f.write_str("context deadline exceeded"),
            CancelCause::Error(e) => std::fmt::Display::fmt(&**e, f),
        }
    }
}

impl CancelCause {
    /// The cause as the error a caller would return for it: `Canceled` → `AgentError::Cancelled`,
    /// `DeadlineExceeded` → `AgentError::DeadlineExceeded`, `Error(e)` →
    /// `AgentError::Other(SharedCauseError(e))` (the same `Arc`, so `Arc::ptr_eq` after a downcast
    /// identifies the same error value).
    pub fn to_agent_error(&self) -> AgentError {
        match self {
            CancelCause::Canceled => AgentError::Cancelled,
            CancelCause::DeadlineExceeded => AgentError::DeadlineExceeded,
            CancelCause::Error(e) => AgentError::Other(Box::new(SharedCauseError(e.clone()))),
        }
    }
}

/// A [`CancelCause::Error`] value as a boxed error (e.g. inside `AgentError::Other`), so the error
/// returned by a run and the cause recorded on its context are the same `Arc`. Display-transparent:
/// it writes the error's text, and `source()` is the error's own source.
#[derive(Debug, Clone)]
pub struct SharedCauseError(pub Arc<dyn std::error::Error + Send + Sync>);

impl std::fmt::Display for SharedCauseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&*self.0, f)
    }
}

impl std::error::Error for SharedCauseError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        std::error::Error::source(&*self.0)
    }
}

/// One cancellable layer of a context: its token, the cause
/// recorded when it was cancelled through the scope, and the scope it was derived from.
#[derive(Debug)]
pub(crate) struct CancelScope {
    pub(crate) token: CancellationToken,
    cause: parking_lot::Mutex<Option<CancelCause>>,
    parent: Option<Arc<CancelScope>>,
}

impl CancelScope {
    /// A parentless scope over `token` (the builder's token, or a raw delta token).
    pub(crate) fn root(token: CancellationToken) -> Arc<CancelScope> {
        Arc::new(CancelScope {
            token,
            cause: parking_lot::Mutex::new(None),
            parent: None,
        })
    }

    /// A child scope over a child token of `parent`'s.
    pub(crate) fn child(parent: &Arc<CancelScope>) -> Arc<CancelScope> {
        Arc::new(CancelScope {
            token: parent.token.child_token(),
            cause: parking_lot::Mutex::new(None),
            parent: Some(parent.clone()),
        })
    }

    /// Cancels the scope with `cause` unless its token is already cancelled (first cause wins).
    pub(crate) fn cancel_with_cause(&self, cause: CancelCause) {
        let mut slot = self.cause.lock();
        if self.token.is_cancelled() {
            return;
        }
        *slot = Some(cause);
        // Cancelling only wakes waiters (no callbacks run), so holding the lock here is fine.
        self.token.cancel();
    }

    /// The cause: `None` while the token is live; else the first recorded cause walking
    /// outward through cancelled scopes, else `Canceled`.
    pub(crate) fn cause(&self) -> Option<CancelCause> {
        if !self.token.is_cancelled() {
            return None;
        }
        let mut s = Some(self);
        while let Some(scope) = s {
            if !scope.token.is_cancelled() {
                break;
            }
            if let Some(c) = scope.cause.lock().clone() {
                return Some(c);
            }
            s = scope.parent.as_deref();
        }
        Some(CancelCause::Canceled)
    }
}

/// Cancels its derived context (cause `Canceled`) when dropped or when [`CancelGuard::cancel`] is
/// called; keep it alive while the derived context is used, or give the
/// context up without cancelling it with [`CancelGuard::release`].
pub struct CancelGuard {
    pub(crate) scope: Option<Arc<CancelScope>>,
    pub(crate) timer: Option<tokio::task::JoinHandle<()>>,
}

impl CancelGuard {
    /// Cancels the derived context with cause `Canceled` (a no-op if it is already cancelled).
    pub fn cancel(&self) {
        if let Some(s) = &self.scope {
            s.cancel_with_cause(CancelCause::Canceled);
        }
    }

    /// Gives the derived context up without cancelling it:
    /// the context stays live until its parent ends, and an armed timeout timer keeps running
    /// (then records `DeadlineExceeded`).
    pub fn release(mut self) {
        self.scope = None;
        // Dropping a JoinHandle detaches the task; it is not aborted.
        self.timer = None;
    }
}

impl Drop for CancelGuard {
    fn drop(&mut self) {
        if let Some(s) = self.scope.take() {
            s.cancel_with_cause(CancelCause::Canceled);
        }
        if let Some(t) = self.timer.take() {
            t.abort();
        }
    }
}

impl std::fmt::Debug for CancelGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CancelGuard")
            .field(
                "cancelled",
                &self.scope.as_ref().is_some_and(|s| s.token.is_cancelled()),
            )
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::error::Error as _;
    use std::sync::Arc;
    use std::time::Duration;

    use tokio_util::sync::CancellationToken;

    use super::{CancelCause, SharedCauseError};
    use crate::context::builder::test_support::named_agent;
    use crate::context::{Context, ContextDelta};
    use crate::error::AgentError;

    fn ctx() -> Context {
        Context::builder().agent(named_agent("a")).build()
    }

    fn err(msg: &str) -> Arc<dyn std::error::Error + Send + Sync> {
        Arc::new(AgentError::other(msg.to_string()))
    }

    /// The cause as a comparable tag: variant, plus the error text.
    fn tag(c: Option<CancelCause>) -> Option<String> {
        c.map(|c| match c {
            CancelCause::Canceled => "Canceled".to_string(),
            CancelCause::DeadlineExceeded => "DeadlineExceeded".to_string(),
            CancelCause::Error(e) => format!("Error({e})"),
        })
    }

    #[test]
    fn display_is_go_text() {
        assert_eq!(CancelCause::Canceled.to_string(), "context canceled");
        assert_eq!(
            CancelCause::DeadlineExceeded.to_string(),
            "context deadline exceeded"
        );
        assert_eq!(CancelCause::Error(err("boom")).to_string(), "boom");
    }

    #[test]
    fn live_context_has_no_cause_and_a_plain_cancel_records_canceled() {
        let parent = ctx();
        let (child, guard) = parent.with_agent_cancel();
        assert!(tag(child.cause()).is_none());
        guard.cancel();
        assert_eq!(tag(child.cause()).as_deref(), Some("Canceled"));
        assert!(tag(parent.cause()).is_none(), "the parent stays live");
    }

    #[test]
    fn first_cause_wins() {
        let (child, guard) = ctx().with_agent_cancel();
        child.cancel_with_cause(CancelCause::Error(err("first")));
        child.cancel_with_cause(CancelCause::DeadlineExceeded);
        guard.cancel();
        drop(guard);
        assert_eq!(tag(child.cause()).as_deref(), Some("Error(first)"));
    }

    #[test]
    fn a_child_cancelled_only_through_its_parent_reads_the_parents_cause() {
        let root = ctx();
        let (parent, _pg) = root.with_agent_cancel();
        let (child, _cg) = parent.with_agent_cancel();
        let (grandchild, _gg) = child.with_agent_cancel();
        let e = err("branch failed");
        parent.cancel_with_cause(CancelCause::Error(e.clone()));
        assert!(child.cancellation_token().is_cancelled());
        let Some(CancelCause::Error(got)) = grandchild.cause() else {
            panic!("grandchild cause: {:?}", tag(grandchild.cause()))
        };
        assert!(Arc::ptr_eq(&got, &e), "the same shared error value");
        assert!(tag(root.cause()).is_none());
    }

    #[test]
    fn a_child_cancelled_plainly_before_its_parent_fails_keeps_canceled() {
        let (parent, _pg) = ctx().with_agent_cancel();
        let (child, cg) = parent.with_agent_cancel();
        drop(cg);
        parent.cancel_with_cause(CancelCause::Error(err("later")));
        assert_eq!(tag(child.cause()).as_deref(), Some("Canceled"));
        assert_eq!(tag(parent.cause()).as_deref(), Some("Error(later)"));
    }

    #[test]
    fn deltas_share_the_scope_and_a_raw_token_starts_a_parentless_one() {
        let (scoped, _g) = ctx().with_agent_cancel();
        let tool = scoped.with_delta(ContextDelta {
            function_call_id: Some("fc".into()),
            agent: Some(named_agent("b")),
            ..Default::default()
        });
        let raw = CancellationToken::new();
        let detached = scoped.with_delta(ContextDelta {
            cancellation_token: Some(raw.clone()),
            ..Default::default()
        });
        tool.cancel_with_cause(CancelCause::DeadlineExceeded);
        assert_eq!(tag(scoped.cause()).as_deref(), Some("DeadlineExceeded"));
        assert!(
            tag(detached.cause()).is_none(),
            "a raw token is not a child"
        );
        raw.cancel();
        assert_eq!(
            tag(detached.cause()).as_deref(),
            Some("Canceled"),
            "a raw token.cancel() records nothing and reads Canceled"
        );
    }

    #[test]
    fn a_raw_cancel_of_a_child_token_reads_the_nearest_recorded_ancestor_cause() {
        let (parent, _pg) = ctx().with_agent_cancel();
        let (child, _cg) = parent.with_agent_cancel();
        // A raw cancel of the child's token records nothing; its parent is live, so Canceled.
        child.cancellation_token().cancel();
        assert_eq!(tag(child.cause()).as_deref(), Some("Canceled"));
        parent.cancel_with_cause(CancelCause::Error(err("e")));
        assert_eq!(tag(child.cause()).as_deref(), Some("Error(e)"));
    }

    #[test]
    fn dropping_the_guard_cancels_and_release_does_not() {
        let parent = ctx();
        let (dropped, g1) = parent.with_agent_cancel();
        let (released, g2) = parent.with_agent_cancel();
        drop(g1);
        g2.release();
        assert_eq!(tag(dropped.cause()).as_deref(), Some("Canceled"));
        assert!(!released.cancellation_token().is_cancelled());
        assert!(tag(released.cause()).is_none());
    }

    #[test]
    fn a_released_context_ends_with_its_parent_and_reads_its_cause() {
        let (parent, _pg) = ctx().with_agent_cancel();
        let (released, g) = parent.with_agent_cancel();
        g.release();
        parent.cancel_with_cause(CancelCause::DeadlineExceeded);
        assert!(released.cancellation_token().is_cancelled());
        assert_eq!(tag(released.cause()).as_deref(), Some("DeadlineExceeded"));
    }

    #[test]
    fn zero_timeout_cancels_at_once_with_deadline_exceeded() {
        let parent = ctx();
        let (child, guard) = parent.with_agent_timeout(Duration::ZERO);
        assert!(child.cancellation_token().is_cancelled());
        drop(guard);
        assert_eq!(tag(child.cause()).as_deref(), Some("DeadlineExceeded"));
        assert!(tag(parent.cause()).is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn the_timeout_timer_records_deadline_exceeded() {
        let parent = ctx();
        let (child, _guard) = parent.with_agent_timeout(Duration::from_secs(5));
        tokio::time::sleep(Duration::from_secs(4)).await;
        assert!(tag(child.cause()).is_none());
        child.cancellation_token().cancelled().await;
        assert_eq!(tag(child.cause()).as_deref(), Some("DeadlineExceeded"));
        assert!(tag(parent.cause()).is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn a_released_timeout_still_fires_with_deadline_exceeded() {
        let (child, guard) = ctx().with_agent_timeout(Duration::from_secs(5));
        guard.release();
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert!(
            !child.cancellation_token().is_cancelled(),
            "release keeps it live"
        );
        child.cancellation_token().cancelled().await;
        assert_eq!(tag(child.cause()).as_deref(), Some("DeadlineExceeded"));
    }

    #[tokio::test(start_paused = true)]
    async fn dropping_a_timeout_guard_cancels_plainly_and_stops_the_timer() {
        let (child, guard) = ctx().with_agent_timeout(Duration::from_secs(5));
        let timer = guard.timer.as_ref().map(|t| t.abort_handle()).unwrap();
        drop(guard);
        assert_eq!(tag(child.cause()).as_deref(), Some("Canceled"));
        tokio::time::sleep(Duration::from_secs(10)).await;
        assert!(timer.is_finished());
        assert_eq!(tag(child.cause()).as_deref(), Some("Canceled"));
    }

    #[tokio::test(start_paused = true)]
    async fn the_timer_ends_when_the_context_is_cancelled_otherwise() {
        let (parent, pg) = ctx().with_agent_cancel();
        let (child, guard) = parent.with_agent_timeout(Duration::from_secs(60));
        let timer = guard.timer.as_ref().map(|t| t.abort_handle()).unwrap();
        guard.release();
        pg.cancel();
        tokio::time::sleep(Duration::from_millis(1)).await;
        assert!(
            timer.is_finished(),
            "a released timer does not outlive its context"
        );
        assert_eq!(tag(child.cause()).as_deref(), Some("Canceled"));
    }

    #[test]
    fn to_agent_error_maps_go_errors_and_shares_the_error_value() {
        assert!(matches!(
            CancelCause::Canceled.to_agent_error(),
            AgentError::Cancelled
        ));
        assert!(matches!(
            CancelCause::DeadlineExceeded.to_agent_error(),
            AgentError::DeadlineExceeded
        ));
        let inner: Arc<dyn std::error::Error + Send + Sync> =
            Arc::new(SharedCauseError(err("root")));
        let e = CancelCause::Error(inner.clone()).to_agent_error();
        assert_eq!(e.to_string(), "root");
        let AgentError::Other(boxed) = &e else {
            panic!("not Other: {e:?}")
        };
        let shared = boxed
            .downcast_ref::<SharedCauseError>()
            .expect("SharedCauseError");
        assert!(Arc::ptr_eq(&shared.0, &inner));
    }

    #[test]
    fn shared_cause_error_delegates_display_and_source() {
        #[derive(Debug)]
        struct Outer(AgentError);
        impl std::fmt::Display for Outer {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "outer: {}", self.0)
            }
        }
        impl std::error::Error for Outer {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                Some(&self.0)
            }
        }
        let shared = SharedCauseError(Arc::new(Outer(AgentError::other("root cause"))));
        assert_eq!(shared.to_string(), "outer: root cause");
        assert_eq!(
            shared.source().map(|s| s.to_string()).as_deref(),
            Some("root cause")
        );
    }
}
