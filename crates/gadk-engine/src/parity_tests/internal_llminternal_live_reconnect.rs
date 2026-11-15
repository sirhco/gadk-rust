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

/// The flow-level tests of Go live_reconnect_test.go: RunLive over the fake Live server,
/// in real time with Go's shrunken policies.
mod flow {
    use std::time::Duration;

    use futures::StreamExt;
    use gadk_agent::AgentError;
    use tokio::sync::oneshot;

    use crate::llm::live::flow::run_live;
    use crate::llm::live::reconnect::{
        DEFAULT_LIVE_RECONNECT_HEALTHY_UPTIME, is_live_reconnect_exhausted,
    };
    use crate::llm::live::test_server::{
        DRAIN_TIMEOUT, FakeConn, FakeLiveServer, SERVER_CONTENT_PONG, assert_no_run_live_leak,
        await_drain, live_ctx, live_flow, probe_of, serve, start_drain, test_live_reconnect_policy,
        wait_for_backoff,
    };

    fn text(e: &Option<AgentError>) -> String {
        e.as_ref().map(ToString::to_string).unwrap_or_default()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_run_live_bounds_a_dead_endpoint() {
        // Hard close straight after setupComplete.
        let server = FakeLiveServer::start(serve(|_n: usize, _conn: FakeConn| async {})).await;
        let flow = live_flow(
            server.model().await,
            Some(test_live_reconnect_policy(4, 20)),
        );
        let (ctx, _cancel) = live_ctx();
        let (sess, stream) = run_live(flow, ctx).unwrap();

        let d = await_drain(start_drain(stream)).await;
        let _ = sess.close().await;

        let err = d.last_err.as_ref().expect("caller got no error");
        assert!(
            is_live_reconnect_exhausted(err),
            "caller got err {err}, want one wrapping the reconnect-exhausted error"
        );
        assert!(
            err.to_string().contains("consecutive"),
            "err {err} does not name the consecutive-attempt bound"
        );
        // One initial dial plus maxAttempts retries.
        assert_eq!(server.conn_count(), 5, "dial count");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_run_live_bounds_a_one_frame_per_connection_backend() {
        let server = FakeLiveServer::start(serve(|_n: usize, mut conn: FakeConn| async move {
            let _ = conn.write_text(SERVER_CONTENT_PONG).await;
            // Give the flow time to consume the frame and reset the consecutive budget.
            tokio::time::sleep(Duration::from_millis(20)).await;
        }))
        .await;
        let flow = live_flow(server.model().await, Some(test_live_reconnect_policy(4, 6)));
        let (ctx, _cancel) = live_ctx();
        let (sess, stream) = run_live(flow, ctx).unwrap();

        let d = await_drain(start_drain(stream)).await;
        let _ = sess.close().await;

        let err = d.last_err.as_ref().expect("caller got no error");
        assert!(
            is_live_reconnect_exhausted(err),
            "caller got err {err}, want one wrapping the reconnect-exhausted error"
        );
        assert!(
            err.to_string().contains("short-lived connections"),
            "err {err} does not name the invocation-wide bound"
        );
        // 1 initial dial + maxTotal retries.
        assert_eq!(server.conn_count(), 7, "dial count");
        assert!(
            d.events > 0,
            "caller received no events, so the reset path was never taken"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_run_live_reconnect_survives_a_transient_drop() {
        let server = FakeLiveServer::start(serve(|n: usize, mut conn: FakeConn| async move {
            if n == 1 {
                return; // one abrupt drop
            }
            if !conn.write_text(SERVER_CONTENT_PONG).await {
                return;
            }
            conn.block_until_client_closes().await;
        }))
        .await;
        let flow = live_flow(
            server.model().await,
            Some(test_live_reconnect_policy(4, 20)),
        );
        let (ctx, _cancel) = live_ctx();
        let (sess, mut stream) = run_live(flow, ctx).unwrap();

        // Key on content, not on any event: the dropped connection still yields a content-less
        // event for the setup exchange.
        let (saw_tx, saw_rx) = oneshot::channel::<()>();
        let drain = tokio::spawn(async move {
            let mut saw = Some(saw_tx);
            let mut stream_err = None;
            while let Some(item) = stream.next().await {
                match item {
                    Err(e) => stream_err = Some(e),
                    Ok(ev) => {
                        if ev.response.content.is_some()
                            && let Some(tx) = saw.take()
                        {
                            let _ = tx.send(());
                        }
                    }
                }
            }
            stream_err
        });
        tokio::time::timeout(DRAIN_TIMEOUT, saw_rx)
            .await
            .expect("never served a turn after the transient drop")
            .unwrap();
        sess.close().await.unwrap();
        let stream_err = drain.await.unwrap();

        assert!(
            stream_err.is_none(),
            "caller got an error after a recoverable drop: {}",
            text(&stream_err)
        );
        assert_eq!(server.conn_count(), 2, "dial count");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_run_live_reconnect_stops_on_teardown() {
        for (name, want_err) in [("close", false), ("cancel", true)] {
            let server = FakeLiveServer::start(serve(|_n: usize, _conn: FakeConn| async {})).await;
            // A backoff far longer than the test, so the flow is parked in the wait.
            let mut p = test_live_reconnect_policy(100, 100);
            p.initial_backoff = Duration::from_secs(3600);
            p.max_backoff = Duration::from_secs(3600);
            let flow = live_flow(server.model().await, Some(p));
            let (ctx, cancel) = live_ctx();
            let (sess, stream) = run_live(flow, ctx).unwrap();
            let probe = probe_of(&sess);

            let drained = start_drain(stream);
            server.wait_for_conns(1).await;
            wait_for_backoff(&probe).await;
            if want_err {
                cancel.cancel();
            } else {
                let _ = sess.close().await;
            }

            let d = await_drain(drained).await;
            if want_err {
                assert!(
                    matches!(d.last_err, Some(AgentError::Cancelled)),
                    "{name}: caller got err {:?}, want context canceled",
                    text(&d.last_err)
                );
            } else {
                assert!(
                    d.last_err.is_none(),
                    "{name}: close reported err {}, want none",
                    text(&d.last_err)
                );
            }
            assert_eq!(
                server.conn_count(),
                1,
                "{name}: a torn-down session must not redial"
            );
            // The stream ends on close whether or not the flow task noticed; this pins the
            // wait as interruptible (an uninterruptible one parks the task for the hour).
            assert_no_run_live_leak(&probe).await;
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_run_live_charges_a_failed_redial_to_the_budget() {
        let server = FakeLiveServer::start(serve(|_n: usize, _conn: FakeConn| async {})).await;
        let flow = live_flow(
            server.model().await,
            Some(test_live_reconnect_policy(3, 20)),
        );
        let (ctx, _cancel) = live_ctx();
        let (sess, stream) = run_live(flow, ctx).unwrap();
        let drained = start_drain(stream);

        server.wait_for_conns(1).await;
        server.close_listener().await; // every later dial is refused

        let d = await_drain(drained).await;
        let _ = sess.close().await;
        let err = d.last_err.as_ref().expect("caller got no error");
        assert!(
            is_live_reconnect_exhausted(err),
            "caller got err {err}, want the reconnect-exhausted error: a refused redial must be charged to the budget, not end the session"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_run_live_first_connect_failure_is_fatal() {
        let server = FakeLiveServer::start(serve(|_n: usize, _conn: FakeConn| async {})).await;
        let model = server.model().await;
        // Returns once the aborted accept task has dropped the listener, before the first dial.
        server.close_listener().await;

        let flow = live_flow(model, Some(test_live_reconnect_policy(5, 20)));
        let (ctx, _cancel) = live_ctx();
        let (sess, stream) = run_live(flow, ctx).unwrap();
        let d = await_drain(start_drain(stream)).await;
        let _ = sess.close().await;

        let err = d
            .last_err
            .as_ref()
            .expect("caller got no error for a connection that never succeeded");
        assert!(
            !is_live_reconnect_exhausted(err),
            "err {err} means the first dial was retried; it must fail fast"
        );
        assert!(
            err.to_string()
                .starts_with("failed to connect live session: "),
            "err {err} lacks Go's connect-failure prefix"
        );
        assert_eq!(server.conn_count(), 0, "accepted connections");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_run_live_ceiling_spares_long_lived_connections() {
        const CONN_LIFETIME: Duration = Duration::from_millis(120);
        let server = FakeLiveServer::start(serve(|_n: usize, mut conn: FakeConn| async move {
            let _ = conn.write_text(SERVER_CONTENT_PONG).await;
            tokio::time::sleep(CONN_LIFETIME).await;
        }))
        .await;
        let mut p = test_live_reconnect_policy(100, 2);
        p.healthy_uptime = CONN_LIFETIME / 4;
        let flow = live_flow(server.model().await, Some(p.clone()));
        let (ctx, _cancel) = live_ctx();
        let (sess, stream) = run_live(flow, ctx).unwrap();
        let drained = start_drain(stream);

        // Long enough for many more cycles than maxTotal.
        tokio::time::sleep(CONN_LIFETIME * 10).await;
        let got = server.conn_count();
        sess.close().await.unwrap();
        let d = await_drain(drained).await;

        assert!(
            d.last_err.is_none(),
            "session ended with {}; connections that served past healthyUptime must not spend the ceiling",
            text(&d.last_err)
        );
        assert!(
            got > p.max_total + 1,
            "dialled {got} times, want more than {}: the ceiling stopped a healthy session",
            p.max_total + 1
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_run_live_ceiling_spares_an_intermediary_cycled_session() {
        // Go intermediaryWebsocketTimeout (25 s) and healthyUptime, both divided by scale = 100.
        let conn_lifetime = Duration::from_secs(25) / 100;
        let server =
            FakeLiveServer::start(serve(move |_n: usize, mut conn: FakeConn| async move {
                let _ = conn.write_text(SERVER_CONTENT_PONG).await;
                tokio::time::sleep(conn_lifetime).await;
            }))
            .await;
        let mut p = test_live_reconnect_policy(100, 2);
        p.healthy_uptime = DEFAULT_LIVE_RECONNECT_HEALTHY_UPTIME / 100;
        let flow = live_flow(server.model().await, Some(p.clone()));
        let (ctx, _cancel) = live_ctx();
        let (sess, stream) = run_live(flow, ctx).unwrap();
        let drained = start_drain(stream);

        // Long enough for more cycles than the ceiling would allow.
        tokio::time::sleep(conn_lifetime * 6).await;
        let got = server.conn_count();
        sess.close().await.unwrap();
        let d = await_drain(drained).await;

        assert!(
            d.last_err.is_none(),
            "session ended with {}; a connection an intermediary cut after {conn_lifetime:?} of service must not spend the ceiling",
            text(&d.last_err)
        );
        assert!(
            got > p.max_total + 1,
            "dialled {got} times, want more than {}: the ceiling ended a session that was serving content",
            p.max_total + 1
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_run_live_consecutive_budget_spares_a_silent_session() {
        const CONN_LIFETIME: Duration = Duration::from_millis(120);
        // No content on any connection, only the setup exchange and a drop.
        let server = FakeLiveServer::start(serve(|_n: usize, conn: FakeConn| async move {
            // Hold the socket for the lifetime (an unused closure argument would be dropped
            // before the future runs, closing the connection at once).
            let _conn = conn;
            tokio::time::sleep(CONN_LIFETIME).await;
        }))
        .await;
        let mut p = test_live_reconnect_policy(2, 1000);
        p.healthy_uptime = CONN_LIFETIME / 4;
        let flow = live_flow(server.model().await, Some(p.clone()));
        let (ctx, _cancel) = live_ctx();
        let (sess, stream) = run_live(flow, ctx).unwrap();
        let drained = start_drain(stream);

        tokio::time::sleep(CONN_LIFETIME * 10).await;
        let got = server.conn_count();
        sess.close().await.unwrap();
        let d = await_drain(drained).await;

        assert!(
            d.last_err.is_none(),
            "session ended with {}; a connection that served past healthyUptime must not spend the consecutive budget",
            text(&d.last_err)
        );
        assert!(
            got > p.max_attempts + 1,
            "dialled {got} times, want more than {}: the consecutive budget killed a silent session",
            p.max_attempts + 1
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_run_live_ceiling_ignores_consumer_latency() {
        // One turnComplete frame per connection, so each connection produces exactly one
        // event. The frame has no content, so it does not reset the consecutive budget.
        let server = FakeLiveServer::start(serve(|_n: usize, mut conn: FakeConn| async move {
            let _ = conn
                .write_text(r#"{"serverContent":{"turnComplete":true}}"#)
                .await;
        }))
        .await;
        // maxAttempts well above maxTotal, so the ceiling is what fires.
        let mut p = test_live_reconnect_policy(100, 3);
        p.healthy_uptime = Duration::from_millis(20);
        let slow = p.healthy_uptime * 10;
        let flow = live_flow(server.model().await, Some(p));
        let (ctx, _cancel) = live_ctx();
        let (sess, mut stream) = run_live(flow, ctx).unwrap();

        let drain = tokio::spawn(async move {
            let mut stream_err = None;
            while let Some(item) = stream.next().await {
                if let Err(e) = item {
                    stream_err = Some(e);
                }
                // Far slower than healthyUptime, which is the whole point.
                tokio::time::sleep(slow).await;
            }
            stream_err
        });
        let stream_err = tokio::time::timeout(DRAIN_TIMEOUT, drain)
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "still reconnecting after {DRAIN_TIMEOUT:?} ({} dials): the caller's latency is being credited as connection uptime",
                    server.conn_count()
                )
            })
            .unwrap();
        let _ = sess.close().await;
        // Name the bound, not just the sentinel: with the clock read at the wrong place the
        // session ends far later on the consecutive-attempt bound instead.
        assert!(
            text(&stream_err).contains("short-lived connections"),
            "session ended with {}, want the short-lived-connection ceiling to fire",
            text(&stream_err)
        );
    }
}
