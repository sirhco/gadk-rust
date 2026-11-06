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

//! Parity port of Go internal/llminternal/live_reconnect_test.go.

use std::collections::HashSet;
use std::time::Duration;

use gadk_agent::LiveSession;
use tokio_util::sync::CancellationToken;

use crate::llm::live::reconnect::{
    DEFAULT_LIVE_RECONNECT_HEALTHY_UPTIME, LiveReconnectPolicy, torn_down, wait_before_reconnect,
};
use crate::llm::live::session::LiveSessionImpl;

/// Go intermediaryWebsocketTimeout: how long a working websocket survives behind an
/// intermediary that caps total connection duration (a Google Cloud load balancer's default
/// backend timeout is 30 s; sessions are cut a little under that).
pub(super) const INTERMEDIARY_WEBSOCKET_TIMEOUT: Duration = Duration::from_secs(25);
/// Go intermediaryHeadroom: the margin healthyUptime keeps from that interval.
pub(super) const INTERMEDIARY_HEADROOM: u32 = 4;

fn policy_with_max_backoff(max_backoff: Duration) -> LiveReconnectPolicy {
    LiveReconnectPolicy {
        initial_backoff: Duration::ZERO,
        max_backoff,
        jitter: 0.0,
        max_attempts: 0,
        max_total: 0,
        healthy_uptime: Duration::ZERO,
    }
}

// parity: adapted: Go's math.MaxInt64-nanosecond Duration is Rust's Duration::MAX, so the wrap case uses Duration::MAX - 1ns
#[test]
fn test_live_reconnect_policy_next_backoff() {
    let cases = [
        (
            "doubles",
            Duration::from_secs(60),
            Duration::from_millis(250),
            Duration::from_millis(500),
        ),
        (
            "saturates at the cap",
            Duration::from_secs(5),
            Duration::from_secs(4),
            Duration::from_secs(5),
        ),
        (
            "saturates rather than overshooting the cap",
            Duration::from_secs(5),
            Duration::from_secs(3),
            Duration::from_secs(5),
        ),
        (
            "a delay at the top of the range cannot wrap",
            Duration::MAX,
            Duration::MAX - Duration::from_nanos(1),
            Duration::MAX,
        ),
    ];
    for (name, max_backoff, input, want) in cases {
        let got = policy_with_max_backoff(max_backoff).next_backoff(input);
        assert_eq!(
            got, want,
            "{name}: nextBackoff({input:?}) = {got:?}, want {want:?}"
        );
    }
}

// parity: adapted: the "negative delay" subtest has no Rust counterpart because std::time::Duration cannot be negative
#[test]
fn test_live_reconnect_policy_jittered() {
    // stays within the band and varies
    let p = LiveReconnectPolicy {
        jitter: 0.2,
        ..policy_with_max_backoff(Duration::ZERO)
    };
    let base = Duration::from_secs(1);
    let (lo, hi) = (base.mul_f64(0.8), base.mul_f64(1.2));
    let mut seen = HashSet::new();
    for _ in 0..200 {
        let got = p.jittered(base);
        assert!(
            got >= lo && got <= hi,
            "jittered({base:?}) = {got:?}, want within [{lo:?}, {hi:?}]"
        );
        seen.insert(got);
    }
    assert!(
        seen.len() >= 2,
        "jittered returned a single value: delays are not being spread"
    );

    // degenerate cases
    let no_jitter = policy_with_max_backoff(Duration::ZERO);
    assert_eq!(
        no_jitter.jittered(Duration::from_secs(1)),
        Duration::from_secs(1),
        "no jitter is exact"
    );
    assert_eq!(p.jittered(Duration::ZERO), Duration::ZERO, "zero delay");
}

#[test]
fn test_default_live_reconnect_policy() {
    let p = LiveReconnectPolicy::default();
    assert!(
        p.max_attempts > 0 && p.max_total > 0,
        "policy has no bound: maxAttempts={} maxTotal={}",
        p.max_attempts,
        p.max_total
    );
    assert!(
        !p.initial_backoff.is_zero(),
        "initialBackoff = {:?}: a non-positive delay restores the hot loop",
        p.initial_backoff
    );
    assert!(
        p.jitter > 0.0,
        "jitter = {}: without spread every client redials in lockstep",
        p.jitter
    );
    // The cap has to be reachable within the consecutive budget.
    let mut d = p.initial_backoff;
    for _ in 0..p.max_attempts - 1 {
        d = p.next_backoff(d);
    }
    assert!(
        d >= p.max_backoff,
        "backoff reaches only {d:?} in {} attempts, so maxBackoff {:?} is unreachable",
        p.max_attempts,
        p.max_backoff
    );
    // The worst case a caller waits before learning the session is dead.
    let mut worst = Duration::ZERO;
    let mut d = p.initial_backoff;
    for _ in 0..p.max_attempts {
        worst += d;
        d = p.next_backoff(d);
    }
    assert!(
        worst <= Duration::from_secs(30),
        "worst-case reconnect window is {worst:?}"
    );
    // healthyUptime sits between an intermediary's cycle and a hang-up-per-frame backend.
    assert!(
        p.healthy_uptime * INTERMEDIARY_HEADROOM <= INTERMEDIARY_WEBSOCKET_TIMEOUT,
        "healthyUptime = {:?}, want at most {:?}",
        p.healthy_uptime,
        INTERMEDIARY_WEBSOCKET_TIMEOUT / INTERMEDIARY_HEADROOM
    );
    assert!(
        p.healthy_uptime > p.initial_backoff * INTERMEDIARY_HEADROOM,
        "healthyUptime = {:?}, want well above initialBackoff {:?}",
        p.healthy_uptime,
        p.initial_backoff
    );
    assert_eq!(p.healthy_uptime, DEFAULT_LIVE_RECONNECT_HEALTHY_UPTIME);
}

// parity: adapted: Go passes the session and a context.Context; Rust passes the invocation's CancellationToken and the session
#[tokio::test]
async fn test_torn_down() {
    // open session
    assert!(
        !torn_down(&CancellationToken::new(), &LiveSessionImpl::new()),
        "tornDown = true for a live session and an uncancelled context"
    );
    // closed session
    let sess = LiveSessionImpl::new();
    sess.close().await.unwrap();
    assert!(
        torn_down(&CancellationToken::new(), &sess),
        "tornDown = false after Close"
    );
    // cancelled context
    let ctx = CancellationToken::new();
    ctx.cancel();
    assert!(
        torn_down(&ctx, &LiveSessionImpl::new()),
        "tornDown = false after the invocation was cancelled"
    );
}

// parity: adapted: Go passes the session and a context.Context; Rust passes the invocation's CancellationToken and the session
#[tokio::test]
async fn test_wait_before_reconnect() {
    // proceeds when nothing tore down
    assert!(
        wait_before_reconnect(
            &CancellationToken::new(),
            &LiveSessionImpl::new(),
            Duration::from_millis(1)
        )
        .await,
        "waitBeforeReconnect = false with a live session and an uncancelled context"
    );
    const FLIPS: usize = 200;
    // refuses a closed session even when the timer is already ready
    for i in 0..FLIPS {
        let sess = LiveSessionImpl::new();
        sess.close().await.unwrap();
        assert!(
            !wait_before_reconnect(&CancellationToken::new(), &sess, Duration::ZERO).await,
            "iteration {i}: proceeded with a closed session"
        );
    }
    // refuses a cancelled context even when the timer is already ready
    for i in 0..FLIPS {
        let ctx = CancellationToken::new();
        ctx.cancel();
        assert!(
            !wait_before_reconnect(&ctx, &LiveSessionImpl::new(), Duration::ZERO).await,
            "iteration {i}: proceeded with a cancelled invocation"
        );
    }
}
