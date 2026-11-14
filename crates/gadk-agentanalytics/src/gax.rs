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

//! The Google Cloud Go client-library pieces the transports share (ports of gax-go v2
//! `Backoff` / `Sleep`, `cloud.google.com/go/internal`'s `wrappedCallErr` and the bigquery
//! client's `randomID`): `bigquery::rest` (`internal.Retry`), `write::gapic` (gax `Invoke`
//! retries) and `offload::gcs` (gensupport `sendAndRetry` / chunk retries).

use std::time::Duration;

use gadk_platform::Platform;

use crate::context::Context;
use crate::error::Error;

/// gax `Backoff`'s settings: the pause before retry *n* is `1 + rand(cur)` ns (jittered), then
/// `cur = min(cur * multiplier, max)`. Zero settings take gax's defaults ([`Pauses::new`]).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub(crate) struct Backoff {
    /// gax `Initial`.
    pub(crate) initial: Duration,
    /// gax `Max`.
    pub(crate) max: Duration,
    /// gax `Multiplier`.
    pub(crate) multiplier: f64,
}

/// A running gax `Backoff` (its `cur`).
#[derive(Debug, Clone)]
pub(crate) struct Pauses {
    backoff: Backoff,
    cur: Duration,
}

impl Pauses {
    /// A fresh backoff with gax's defaults for zero settings (`Initial` 1s, `Max` 30s,
    /// `Multiplier` < 1 → 2).
    pub(crate) fn new(mut backoff: Backoff) -> Pauses {
        if backoff.initial.is_zero() {
            backoff.initial = Duration::from_secs(1);
        }
        if backoff.max.is_zero() {
            backoff.max = Duration::from_secs(30);
        }
        if backoff.multiplier < 1.0 {
            backoff.multiplier = 2.0;
        }
        Pauses {
            backoff,
            cur: backoff.initial,
        }
    }

    /// gax `Backoff.Pause()` with `rand(n)` uniform in `[0, n)` (Go `rand.Int63n`).
    pub(crate) fn pause(&mut self) -> Duration {
        self.next(|n| rand::random_range(0..n))
    }

    /// gax `Backoff.Pause()`: `1 + rand(cur)` ns, then `cur` grows as Go's
    /// `time.Duration(float64(cur) * Multiplier)`, capped at `Max`.
    pub(crate) fn next(&mut self, rand: impl FnOnce(u64) -> u64) -> Duration {
        let cur = u64::try_from(self.cur.as_nanos())
            .unwrap_or(u64::MAX)
            .max(1);
        let pause = Duration::from_nanos(1 + rand(cur));
        let grown = Duration::from_nanos((cur as f64 * self.backoff.multiplier) as u64);
        self.cur = grown.min(self.backoff.max);
        pause
    }
}

/// gax-go `Sleep(ctx, d)`: waits `d`, or fails with the context error once `ctx` ends.
pub(crate) async fn sleep(ctx: &Context, d: Duration) -> Result<(), Error> {
    ctx.run(async {
        tokio::time::sleep(d).await;
        Ok(())
    })
    .await
}

/// Go `wrappedCallErr{ctxErr, wrappedErr}`: `retry failed with <ctx err>; last error: <err>`,
/// both errors in its tree (Go's `Is` matches either; `Unwrap` returns the call's error).
pub(crate) fn wrapped_call_err(ctx_err: Error, last: Error) -> Error {
    Error::wrap_text(
        format!("retry failed with {ctx_err}; last error: {last}"),
        Error::Joined(vec![ctx_err, last]),
    )
}

/// Go `randomID()`: 16 random bytes as 32 lower-case hex digits (from a random v4 UUID, whose
/// version and variant digits are fixed — the ID keeps Go's shape). Also the storage client's
/// idempotency tokens and the multipart boundary's digits.
pub(crate) fn random_id() -> String {
    // Divergence (ledger 7b-1): Go draws all 128 bits from `crypto/rand`; a v4 UUID fixes 6 of
    // them. The ID's shape (`^[0-9a-f]{32}$`) is Go's.
    Platform::default().new_uuid().replace('-', "")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use pretty_assertions::assert_eq;

    use super::*;
    use crate::context::ContextError;

    #[test]
    fn pauses_follow_gax_backoff() {
        let mut p = Pauses::new(Backoff {
            initial: Duration::from_millis(100),
            max: Duration::from_millis(200),
            multiplier: 1.3,
        });
        // rand(n) = n - 1: the largest pause, cur itself.
        assert_eq!(p.next(|n| n - 1), Duration::from_millis(100));
        assert_eq!(p.next(|n| n - 1), Duration::from_millis(130));
        assert_eq!(p.next(|n| n - 1), Duration::from_millis(169));
        assert_eq!(p.next(|n| n - 1), Duration::from_millis(200));
        assert_eq!(p.next(|_| 0), Duration::from_nanos(1));
        let mut zero = Pauses::new(Backoff {
            initial: Duration::ZERO,
            max: Duration::ZERO,
            multiplier: 0.5,
        });
        assert_eq!(zero.next(|n| n - 1), Duration::from_secs(1));
        assert_eq!(zero.next(|n| n - 1), Duration::from_secs(2));
        let mut defaults = Pauses::new(Backoff::default());
        let caps: Vec<u64> = (0..7).map(|_| defaults.next(|n| n - 1).as_secs()).collect();
        assert_eq!(caps, [1, 2, 4, 8, 16, 30, 30]);
    }

    #[test]
    fn random_pauses_stay_within_gax_bounds() {
        let mut p = Pauses::new(Backoff {
            initial: Duration::from_millis(100),
            ..Backoff::default()
        });
        let mut cap = Duration::from_millis(100);
        for _ in 0..12 {
            let d = p.pause();
            assert!(d >= Duration::from_nanos(1) && d <= cap, "{d:?} > {cap:?}");
            cap = (cap * 2).min(Duration::from_secs(30));
        }
    }

    #[test]
    fn wrapped_call_errors_have_gos_text_and_tree() {
        let e = wrapped_call_err(
            Error::Context(ContextError::DeadlineExceeded),
            Error::msg("boom"),
        );
        assert_eq!(
            e.to_string(),
            "retry failed with context deadline exceeded; last error: boom"
        );
        assert!(e.is_context(ContextError::DeadlineExceeded));
    }

    #[test]
    fn random_ids_have_gos_shape() {
        let a = random_id();
        let b = random_id();
        assert_eq!(a.len(), 32);
        assert!(
            a.bytes()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
        assert_ne!(a, b);
    }
}
