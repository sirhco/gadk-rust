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

//! Reconnect policy for the live flow, plus `torn_down` and `wait_before_reconnect`.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gadk_agent::AgentError;
use tokio_util::sync::CancellationToken;

use crate::llm::live::session::{LiveProbe, LiveSessionImpl};

/// Default initial backoff.
pub(crate) const DEFAULT_LIVE_RECONNECT_INITIAL_BACKOFF: Duration = Duration::from_millis(250);
/// Default maximum backoff.
pub(crate) const DEFAULT_LIVE_RECONNECT_MAX_BACKOFF: Duration = Duration::from_secs(2);
/// Default jitter fraction.
pub(crate) const DEFAULT_LIVE_RECONNECT_JITTER: f64 = 0.2;
/// Default maximum consecutive attempts (adk-python DEFAULT_MAX_RECONNECT_ATTEMPTS).
pub(crate) const DEFAULT_LIVE_RECONNECT_MAX_ATTEMPTS: usize = 5;
/// Default maximum short-lived connections over the invocation.
pub(crate) const DEFAULT_LIVE_RECONNECT_MAX_TOTAL: usize = 20;
/// Default uptime after which a connection counts as healthy.
pub(crate) const DEFAULT_LIVE_RECONNECT_HEALTHY_UPTIME: Duration = Duration::from_secs(5);
/// Error text when the reconnect budget is exhausted.
pub(crate) const ERR_LIVE_RECONNECT_EXHAUSTED: &str = "live session: reconnect budget exhausted";

/// Bounds how hard the live flow retries a connection that keeps
/// dropping. `max_attempts` bounds consecutive reconnects (reset by content or by a connection
/// that outlives `healthy_uptime`); `max_total` bounds short-lived connections over the
/// invocation and never resets. Only tests build a non-default policy, to shrink the delays.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LiveReconnectPolicy {
    pub(crate) initial_backoff: Duration,
    /// Caps the delay before jitter is applied.
    pub(crate) max_backoff: Duration,
    /// Spreads each delay over ±jitter; non-positive means no jitter.
    pub(crate) jitter: f64,
    pub(crate) max_attempts: usize,
    pub(crate) max_total: usize,
    pub(crate) healthy_uptime: Duration,
}

impl Default for LiveReconnectPolicy {
    /// The default policy.
    fn default() -> Self {
        Self {
            initial_backoff: DEFAULT_LIVE_RECONNECT_INITIAL_BACKOFF,
            max_backoff: DEFAULT_LIVE_RECONNECT_MAX_BACKOFF,
            jitter: DEFAULT_LIVE_RECONNECT_JITTER,
            max_attempts: DEFAULT_LIVE_RECONNECT_MAX_ATTEMPTS,
            max_total: DEFAULT_LIVE_RECONNECT_MAX_TOTAL,
            healthy_uptime: DEFAULT_LIVE_RECONNECT_HEALTHY_UPTIME,
        }
    }
}

impl LiveReconnectPolicy {
    /// The delay to grow into after waiting `d`, doubling up to `max_backoff`.
    /// Saturating at the cap also keeps the doubling from overflowing.
    pub(crate) fn next_backoff(&self, d: Duration) -> Duration {
        if d >= self.max_backoff / 2 {
            return self.max_backoff;
        }
        d.saturating_mul(2)
    }

    /// Spreads `d` over ±jitter.
    pub(crate) fn jittered(&self, d: Duration) -> Duration {
        if d.is_zero() || self.jitter <= 0.0 {
            return d;
        }
        let factor = 1.0 + (random_unit() * 2.0 - 1.0) * self.jitter;
        Duration::try_from_secs_f64((d.as_secs_f64() * factor).max(0.0)).unwrap_or(Duration::MAX)
    }
}

/// A uniformly distributed value in `[0, 1)` without a `rand` dependency: a
/// freshly, randomly keyed `RandomState` hasher over the current time.
fn random_unit() -> f64 {
    let mut h = RandomState::new().build_hasher();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    h.write_u128(nanos);
    (h.finish() >> 11) as f64 / (1u64 << 53) as f64
}

/// The error the live flow reports when it stopped reconnecting because a
/// bound fired. `bound` names which one; `cause` is the failure behind the last attempt.
#[derive(Debug)]
pub(crate) struct LiveReconnectGaveUp {
    pub(crate) bound: String,
    pub(crate) cause: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl std::fmt::Display for LiveReconnectGaveUp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.cause {
            None => write!(f, "{ERR_LIVE_RECONNECT_EXHAUSTED}: {}", self.bound),
            Some(c) => write!(f, "{ERR_LIVE_RECONNECT_EXHAUSTED}: {}: {c}", self.bound),
        }
    }
}

impl std::error::Error for LiveReconnectGaveUp {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.cause
            .as_deref()
            .map(|c| c as &(dyn std::error::Error + 'static))
    }
}

/// Builds the [`LiveReconnectGaveUp`] error.
pub(crate) fn live_reconnect_gave_up_error(
    bound: String,
    cause: Option<Box<dyn std::error::Error + Send + Sync>>,
) -> AgentError {
    AgentError::Other(Box::new(LiveReconnectGaveUp { bound, cause }))
}

/// Whether the error is a [`LiveReconnectGaveUp`] (the reconnect budget was exhausted).
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "Go matches the sentinel only in tests")
)]
pub(crate) fn is_live_reconnect_exhausted(err: &AgentError) -> bool {
    matches!(err, AgentError::Other(e) if e.downcast_ref::<LiveReconnectGaveUp>().is_some())
}

/// Whether the session was closed or the invocation cancelled.
pub(crate) fn torn_down(ctx_token: &CancellationToken, sess: &LiveSessionImpl) -> bool {
    sess.done().is_cancelled() || ctx_token.is_cancelled()
}

/// Paces a reconnect, reporting whether it should go ahead.
/// Teardown during the wait cuts it short; the re-check after the timer matters because
/// `select!` picks among ready branches at random, so a teardown landing as the timer fires
/// would otherwise report "proceed" half the time.
pub(crate) async fn wait_before_reconnect(
    ctx_token: &CancellationToken,
    sess: &LiveSessionImpl,
    d: Duration,
) -> bool {
    let _in_backoff = BackoffGuard::enter(sess.probe());
    tokio::select! {
        _ = tokio::time::sleep(d) => !torn_down(ctx_token, sess),
        _ = sess.done().cancelled() => false,
        _ = ctx_token.cancelled() => false,
    }
}

/// Raises `LiveProbe::in_backoff` for the lifetime of one wait (tests observe it).
struct BackoffGuard(Arc<LiveProbe>);

impl BackoffGuard {
    fn enter(probe: &Arc<LiveProbe>) -> Self {
        probe.in_backoff.fetch_add(1, Ordering::SeqCst);
        Self(probe.clone())
    }
}

impl Drop for BackoffGuard {
    fn drop(&mut self) {
        self.0.in_backoff.fetch_sub(1, Ordering::SeqCst);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn gave_up_error_text_and_identity() {
        let bare = live_reconnect_gave_up_error(
            "5 consecutive attempts delivered no content".into(),
            None,
        );
        assert_eq!(
            bare.to_string(),
            "live session: reconnect budget exhausted: 5 consecutive attempts delivered no content"
        );
        assert!(is_live_reconnect_exhausted(&bare));
        let with_cause = live_reconnect_gave_up_error(
            "20 short-lived connections in one invocation".into(),
            Some("EOF".into()),
        );
        assert_eq!(
            with_cause.to_string(),
            "live session: reconnect budget exhausted: 20 short-lived connections in one invocation: EOF"
        );
        assert!(is_live_reconnect_exhausted(&with_cause));
        assert!(!is_live_reconnect_exhausted(&AgentError::other(
            ERR_LIVE_RECONNECT_EXHAUSTED
        )));
    }

    use std::task::Poll;

    use futures::poll;
    use gadk_agent::LiveSession;

    // These tests poll the wait by hand, so they never depend on timing.

    #[tokio::test]
    async fn wait_raises_in_backoff_while_waiting() {
        let sess = LiveSessionImpl::new();
        let ctx = CancellationToken::new();
        let mut wait = std::pin::pin!(wait_before_reconnect(
            &ctx,
            &sess,
            Duration::from_secs(3600)
        ));
        assert!(poll!(wait.as_mut()).is_pending());
        assert_eq!(sess.probe().in_backoff.load(Ordering::SeqCst), 1);
        ctx.cancel();
        assert!(
            matches!(poll!(wait.as_mut()), Poll::Ready(false)),
            "cancellation ends the wait with 'do not proceed'"
        );
        assert_eq!(sess.probe().in_backoff.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn close_during_the_wait_cuts_it_short() {
        let sess = LiveSessionImpl::new();
        let ctx = CancellationToken::new();
        let mut wait = std::pin::pin!(wait_before_reconnect(
            &ctx,
            &sess,
            Duration::from_secs(3600)
        ));
        assert!(poll!(wait.as_mut()).is_pending());
        sess.close().await.unwrap();
        assert!(matches!(poll!(wait.as_mut()), Poll::Ready(false)));
        assert_eq!(sess.probe().in_backoff.load(Ordering::SeqCst), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn wait_proceeds_after_the_full_delay() {
        let sess = LiveSessionImpl::new();
        let ctx = CancellationToken::new();
        let start = tokio::time::Instant::now();
        assert!(wait_before_reconnect(&ctx, &sess, Duration::from_millis(750)).await);
        assert_eq!(start.elapsed(), Duration::from_millis(750));
        assert_eq!(sess.probe().in_backoff.load(Ordering::SeqCst), 0);
    }
}
