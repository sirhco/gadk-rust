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

//! Cancellation and failure causes of span records: `was_canceled`, `failure_cause`,
//! `raw_context_failure_cause`, `is_parallel_branch`, `context_failure_cause`,
//! `open_failure_cause`, `open_failure_cause_locked`, `canceled_by_stop`,
//! `collateral_failure_cause`, and the `is_canceled` test on recorded errors.
//!
//! The walks follow `parent` links. The [`SpanRef`] methods lock the record they start from and
//! then each parent; the [`SpanRecord`] methods of the same names start from a record whose guard
//! the caller holds (as `open_call_owners_locked`'s `ignore` gets it) and lock only its parents.
//! Never call either while holding the guard of a parent of the start record.
//!
//! Cause mapping: `CancelSource::check` reads the cause of the callback's context when asked — a
//! plain cancel is [`ContextError::Canceled`], an agent or node timeout
//! [`ContextError::DeadlineExceeded`], a ParallelAgent sibling's error that error.

use std::error::Error as StdError;
use std::fmt;

use super::TraceError;
use super::invocation::InvocationState;
use super::span::{SpanKind, SpanRecord, SpanRef, parallel_branch};
use crate::context::ContextError;
use crate::error::Error;
use gadk_agent::fiber::ERRGROUP_CREATOR;

impl SpanRef {
    /// Whether the callback's context was canceled (see [`SpanRecord::was_canceled`]).
    #[cfg(test)]
    pub(crate) fn was_canceled(&self) -> bool {
        self.lock().was_canceled()
    }

    /// The failure cause (see [`SpanRecord::failure_cause`]).
    #[cfg(test)]
    pub(crate) fn failure_cause(&self) -> Option<TraceError> {
        self.lock().failure_cause()
    }

    /// The raw context failure cause (see [`SpanRecord::raw_context_failure_cause`]).
    #[cfg(test)]
    pub(crate) fn raw_context_failure_cause(&self) -> Option<TraceError> {
        self.lock().raw_context_failure_cause()
    }

    /// Whether this is a parallel branch (with no fiber information: the branch differs from the
    /// parent's).
    #[cfg(test)]
    pub(crate) fn is_parallel_branch(&self) -> bool {
        self.lock().is_parallel_branch()
    }

    /// The context failure cause (see [`SpanRecord::context_failure_cause`]).
    pub(crate) fn context_failure_cause(&self) -> Option<TraceError> {
        self.lock().context_failure_cause()
    }

    /// The open failure cause: the context failure cause, else the first retired cause.
    pub(crate) fn open_failure_cause(&self) -> Option<TraceError> {
        self.lock().open_failure_cause()
    }

    /// Whether the span was canceled by a stop point (see [`SpanRecord::canceled_by_stop`]).
    pub(crate) fn canceled_by_stop(&self) -> bool {
        self.lock().canceled_by_stop()
    }

    /// The collateral failure cause: `(cause, collateral)`.
    #[cfg(test)]
    pub(crate) fn collateral_failure_cause(&self) -> (Option<TraceError>, bool) {
        self.lock().collateral_failure_cause()
    }
}

impl SpanRecord {
    /// Whether the callback's context was canceled with cause [`ContextError::Canceled`].
    pub(crate) fn was_canceled(&self) -> bool {
        self.canceled.as_ref().is_some_and(|c| c.check().0)
    }

    /// The cause other than [`ContextError::Canceled`] with which the callback's context ended.
    pub(crate) fn failure_cause(&self) -> Option<TraceError> {
        self.canceled.as_ref().and_then(|c| c.check().1)
    }

    /// [`SpanRecord::failure_cause`] unless it is bare [`ContextError::DeadlineExceeded`] (a
    /// workflow node or agent timeout).
    pub(crate) fn raw_context_failure_cause(&self) -> Option<TraceError> {
        self.failure_cause()
            .filter(|c| !matches!(**c, Error::Context(ContextError::DeadlineExceeded)))
    }

    /// Whether this span is a parallel branch of its parent agent. Locks the parent.
    pub(crate) fn is_parallel_branch(&self) -> bool {
        if self.kind != SpanKind::Agent {
            return false;
        }
        let Some(p) = self.parent.as_ref() else {
            return false;
        };
        let p = p.lock();
        if self.goroutine_id == 0 {
            return self.branch != p.branch;
        }
        p.is_parallel
            && self.origin.started_by(ERRGROUP_CREATOR, p.goroutine_id)
            && p.children.contains(&self.name)
            && self.branch == parallel_branch(&p, &self.name)
    }

    /// The raw cause, only when the outermost span sharing one is a parallel branch. Locks the
    /// parents.
    pub(crate) fn context_failure_cause(&self) -> Option<TraceError> {
        let cause = self.raw_context_failure_cause()?;
        let top_is_branch = match self.top_with(|p| p.raw_context_failure_cause().is_some()) {
            None => self.is_parallel_branch(),
            Some(top) => top.lock().is_parallel_branch(),
        };
        top_is_branch.then_some(cause)
    }

    /// The open failure cause: the context failure cause, else the first retired cause.
    pub(crate) fn open_failure_cause(&self) -> Option<TraceError> {
        self.context_failure_cause()
            .or_else(|| self.first_retired_cause())
    }

    /// Whether the span was canceled with cause [`ContextError::Canceled`] and the outermost
    /// span so canceled is a stop point. Locks the parents.
    pub(crate) fn canceled_by_stop(&self) -> bool {
        if !self.was_canceled() {
            return false;
        }
        match self.top_with(SpanRecord::was_canceled) {
            None => self.stop_point,
            Some(top) => top.lock().stop_point,
        }
    }

    /// The raw cause, and whether the outermost span sharing one is a stop point. Locks the
    /// parents.
    pub(crate) fn collateral_failure_cause(&self) -> (Option<TraceError>, bool) {
        let Some(cause) = self.raw_context_failure_cause() else {
            return (None, false);
        };
        let collateral = match self.top_with(|p| p.raw_context_failure_cause().is_some()) {
            None => self.stop_point,
            Some(top) => top.lock().stop_point,
        };
        (Some(cause), collateral)
    }

    /// The outermost ancestor reached by walking up while `shared` holds of each parent: `None`
    /// when the walk stays at `self`.
    fn top_with(&self, shared: impl Fn(&SpanRecord) -> bool) -> Option<SpanRef> {
        let mut top = None;
        let mut next = self.parent.clone();
        while let Some(p) = next {
            let parent = {
                let g = p.lock();
                if !shared(&g) {
                    break;
                }
                g.parent.clone()
            };
            top = Some(p);
            next = parent;
        }
        top
    }
}

impl InvocationState {
    /// The first open failure cause on `agent_stack`, most recent first (the invocation span's
    /// retired causes included).
    pub(crate) fn open_failure_cause_locked(&self) -> Option<TraceError> {
        self.agent_stack
            .iter()
            .rev()
            .find_map(SpanRef::open_failure_cause)
    }
}

/// Whether `err` is a canceled-context error anywhere in its tree.
pub(crate) fn is_canceled(err: &Error) -> bool {
    err.is_context(ContextError::Canceled)
}

/// Whether `target` (by identity) is in the error tree of a kept error `err`.
pub(crate) fn error_is(err: &Error, target: &TraceError) -> bool {
    err.find(&mut |e| std::ptr::eq(e, &**target)).is_some()
}

/// The text `"{err}: {cause}"`, with both errors in its tree.
pub(crate) fn wrap_with_cause(err: &TraceError, cause: &TraceError) -> TraceError {
    std::sync::Arc::new(Error::wrap_text(
        format!("{err}: {cause}"),
        Error::Joined(vec![
            Error::Other(Box::new(Shared(err.clone()))),
            Error::Other(Box::new(Shared(cause.clone()))),
        ]),
    ))
}

/// A kept error inside another error's tree: its `source` is the kept [`Error`] itself, so
/// [`Error::find`] walks into it.
#[derive(Debug)]
struct Shared(TraceError);

impl fmt::Display for Shared {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&*self.0, f)
    }
}

impl StdError for Shared {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        Some(&*self.0)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use std::sync::Arc;

    use tokio_util::sync::CancellationToken;

    use super::super::span::{CancelSource, RetiredCause};
    use super::super::testkit::ManualClock;
    use super::*;
    use crate::trace::InvocationTrace;

    fn source(canceled: bool, cause: Option<TraceError>) -> Option<CancelSource> {
        let token = CancellationToken::new();
        if canceled {
            token.cancel();
        }
        Some(CancelSource::of_token(token, cause))
    }

    fn record(kind: SpanKind, parent: Option<&SpanRef>, branch: &str) -> SpanRecord {
        SpanRecord {
            kind,
            parent: parent.cloned(),
            branch: branch.into(),
            ..SpanRecord::default()
        }
    }

    #[test]
    fn canceled_by_stop_starts_at_a_stop_point() {
        let root = SpanRef::new(record(SpanKind::Invocation, None, ""));
        for (agent_canceled, stop_point, want) in [
            (true, true, true),
            (true, false, false),
            (false, true, false),
        ] {
            let agent = SpanRef::new(SpanRecord {
                stop_point,
                canceled: source(agent_canceled, None),
                ..record(SpanKind::Agent, Some(&root), "")
            });
            let call = SpanRef::new(SpanRecord {
                canceled: source(true, None),
                ..record(SpanKind::Llm, Some(&agent), "")
            });
            assert!(call.was_canceled());
            assert_eq!(
                call.canceled_by_stop(),
                want,
                "agent canceled {agent_canceled}, stop point {stop_point}"
            );
        }
        // An explicit `context.Canceled` cause is still a plain cancellation; any other cause
        // is a failure cause.
        let canceled = SpanRef::new(SpanRecord {
            stop_point: true,
            canceled: source(true, Some(Arc::new(Error::Context(ContextError::Canceled)))),
            ..record(SpanKind::Agent, Some(&root), "")
        });
        assert!(canceled.canceled_by_stop());
        assert!(canceled.failure_cause().is_none());
        let failed = SpanRef::new(SpanRecord {
            stop_point: true,
            canceled: source(true, Some(Arc::new(Error::msg("boom")))),
            ..record(SpanKind::Agent, Some(&root), "")
        });
        assert!(!failed.was_canceled() && !failed.canceled_by_stop());
        assert_eq!(failed.failure_cause().unwrap().to_string(), "boom");
        // A live context reports nothing.
        let live = SpanRef::new(SpanRecord {
            canceled: source(false, Some(Arc::new(Error::msg("boom")))),
            ..record(SpanKind::Agent, Some(&root), "")
        });
        assert!(!live.was_canceled() && live.failure_cause().is_none());
    }

    #[test]
    fn context_failure_causes_count_only_on_parallel_branches() {
        let root = SpanRef::new(record(SpanKind::Invocation, None, ""));
        let par = SpanRef::new(record(SpanKind::Agent, Some(&root), "par"));
        let cause: TraceError = Arc::new(Error::msg("branch b failed"));
        // Zero goroutine information: a branch below its parent's counts as parallel.
        let branch = SpanRef::new(SpanRecord {
            name: "b".into(),
            canceled: source(true, Some(cause.clone())),
            ..record(SpanKind::Agent, Some(&par), "par.b")
        });
        assert!(branch.is_parallel_branch());
        assert!(Arc::ptr_eq(
            &branch.context_failure_cause().unwrap(),
            &cause
        ));
        assert!(Arc::ptr_eq(&branch.open_failure_cause().unwrap(), &cause));
        // The same cause on a span of its parent's branch (a custom agent's own
        // WithCancelCause) is ignored.
        let same_branch = SpanRef::new(SpanRecord {
            canceled: source(true, Some(cause.clone())),
            ..record(SpanKind::Agent, Some(&par), "par")
        });
        assert!(!same_branch.is_parallel_branch());
        assert!(same_branch.raw_context_failure_cause().is_some());
        assert!(same_branch.context_failure_cause().is_none());
        // Under a branch sharing the cause, the outermost span decides.
        let under = SpanRef::new(SpanRecord {
            canceled: source(true, Some(cause.clone())),
            ..record(SpanKind::Agent, Some(&branch), "par.b")
        });
        assert!(under.context_failure_cause().is_some());
        // Bare DeadlineExceeded is no failure cause; a retired cause is the fallback.
        let timed_out = SpanRef::new(SpanRecord {
            canceled: source(
                true,
                Some(Arc::new(Error::Context(ContextError::DeadlineExceeded))),
            ),
            retired_causes: vec![RetiredCause {
                err: Some(cause.clone()),
                ..RetiredCause::default()
            }],
            ..record(SpanKind::Agent, Some(&par), "par.c")
        });
        assert!(timed_out.failure_cause().is_some());
        assert!(timed_out.raw_context_failure_cause().is_none());
        assert!(timed_out.context_failure_cause().is_none());
        assert!(Arc::ptr_eq(
            &timed_out.open_failure_cause().unwrap(),
            &cause
        ));
    }

    #[test]
    fn collateral_causes_start_at_a_stop_point() {
        let root = SpanRef::new(record(SpanKind::Invocation, None, ""));
        let cause: TraceError = Arc::new(Error::msg("sibling failed"));
        for stop_point in [true, false] {
            let agent = SpanRef::new(SpanRecord {
                stop_point,
                canceled: source(true, Some(cause.clone())),
                ..record(SpanKind::Agent, Some(&root), "")
            });
            let call = SpanRef::new(SpanRecord {
                canceled: source(true, Some(cause.clone())),
                ..record(SpanKind::Llm, Some(&agent), "")
            });
            let (got, collateral) = call.collateral_failure_cause();
            assert!(Arc::ptr_eq(&got.unwrap(), &cause));
            assert_eq!(collateral, stop_point);
        }
        let live = SpanRef::new(record(SpanKind::Llm, Some(&root), ""));
        assert!(matches!(live.collateral_failure_cause(), (None, false)));
    }

    #[test]
    fn open_failure_cause_locked_scans_most_recent_first() {
        let clock = ManualClock::new();
        let inv = InvocationTrace::new_with_span(None, "trace", clock.now());
        let mut st = inv.mu.lock();
        assert!(st.open_failure_cause_locked().is_none());
        let root = st.agent_stack[0].clone();
        let (older, newer): (TraceError, TraceError) =
            (Arc::new(Error::msg("older")), Arc::new(Error::msg("newer")));
        root.lock().retired_causes.push(RetiredCause {
            err: Some(older.clone()),
            ..RetiredCause::default()
        });
        assert!(Arc::ptr_eq(
            &st.open_failure_cause_locked().unwrap(),
            &older
        ));
        let a = SpanRef::new(SpanRecord {
            retired_causes: vec![RetiredCause {
                err: Some(newer.clone()),
                ..RetiredCause::default()
            }],
            ..record(SpanKind::Agent, Some(&root), "")
        });
        st.agent_stack.push(a);
        assert!(Arc::ptr_eq(
            &st.open_failure_cause_locked().unwrap(),
            &newer
        ));
    }

    #[test]
    fn canceled_errors_and_wrapped_causes() {
        let canceled: TraceError = Arc::new(Error::Context(ContextError::Canceled));
        assert!(is_canceled(&canceled));
        assert!(is_canceled(&Error::wrap(
            "model",
            Error::Context(ContextError::Canceled)
        )));
        assert!(!is_canceled(&Error::Context(
            ContextError::DeadlineExceeded
        )));
        assert!(!is_canceled(&Error::msg("context canceled")));
        let cause: TraceError = Arc::new(Error::msg("budget exhausted"));
        assert!(!error_is(&canceled, &cause));
        assert!(error_is(&cause, &cause));
        let wrapped = wrap_with_cause(&canceled, &cause);
        assert_eq!(wrapped.to_string(), "context canceled: budget exhausted");
        assert!(is_canceled(&wrapped));
        assert!(error_is(&wrapped, &cause));
        assert!(error_is(&wrapped, &canceled));
        // Another error with the same text is not the cause.
        assert!(!error_is(
            &wrapped,
            &Arc::new(Error::msg("budget exhausted"))
        ));
    }
}
