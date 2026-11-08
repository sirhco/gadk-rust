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

//! Port of Go `tool/mcptoolset/client_cancellation_test.go` (package `mcptoolset`; Task 6).
//!
//! Go's `gatedSessionTransport` is [`ScriptedFactory`] with its gate closed: every connect
//! counts itself, announces itself to `entered()`, and waits for `open_gate()` (a dropped
//! connect future stops waiting, as Go's `ctx.Done()` branch does).

use std::sync::Arc;
use std::time::Duration;

use rmcp::RoleClient;
use rmcp::model::ClientRequest;
use rmcp::service::Peer;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::client::{CallContext, ConnectionRefresher, SessionClient};
use crate::config::default_client_info;
use crate::error::Error;
use crate::testutil::{ScriptedFactory, ToolServer};

/// Go `mcp.NewServer(&mcp.Implementation{Name: "test", Version: "1"}, nil)`.
fn test_server() -> ToolServer {
    ToolServer::new("test", "1")
}

/// Go `newConnectionRefresher(nil, transport)` over a gated factory.
fn gated() -> (Arc<ScriptedFactory>, Arc<ConnectionRefresher>) {
    let factory = ScriptedFactory::new(test_server);
    factory.close_gate();
    let c = Arc::new(ConnectionRefresher::new(
        default_client_info(),
        factory.clone(),
    ));
    (factory, c)
}

/// Go `context.WithTimeout`: a context whose token fires after `after`.
fn timeout_cx(after: Duration) -> CallContext {
    let cx = CallContext::background();
    let token = cx.cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(after).await;
        token.cancel();
    });
    cx
}

/// Go `session.Ping(ctx, nil)`.
async fn ping(peer: &Peer<RoleClient>) -> Result<(), rmcp::ServiceError> {
    peer.send_request(ClientRequest::PingRequest(Default::default()))
        .await
        .map(|_| ())
}

/// Whether two peers belong to the same session (Go compares `*mcp.ClientSession` pointers).
fn same_session(a: &Peer<RoleClient>, b: &Peer<RoleClient>) -> bool {
    match (a.peer_info(), b.peer_info()) {
        (Some(a), Some(b)) => Arc::ptr_eq(&a, &b),
        _ => false,
    }
}

/// Go `acquire := c.getSession` / `c.refreshConnection`.
async fn acquire(
    c: Arc<ConnectionRefresher>,
    refresh: bool,
    cx: CallContext,
) -> Result<Peer<RoleClient>, Error> {
    if refresh {
        c.refresh_connection(&cx).await
    } else {
        c.get_session(&cx).await
    }
}

/// One `TestSessionWaitCancellation` subtest.
async fn session_wait_cancellation(refresh: bool, deadline: bool) {
    let name = format!(
        "{}/{}",
        if refresh { "refresh" } else { "initialize" },
        if deadline { "deadline" } else { "cancel" }
    );
    let (transport, c) = gated();
    let owner_cx = timeout_cx(Duration::from_secs(5));
    let mut owner = tokio::spawn(acquire(c.clone(), refresh, owner_cx));
    transport.entered().await;

    assert_eq!(transport.connects(), 1, "{name}: connect count");

    let waiter_cx = if deadline {
        timeout_cx(Duration::from_millis(50))
    } else {
        CallContext::background()
    };
    let waiter = tokio::spawn(acquire(c.clone(), refresh, waiter_cx.clone()));

    if deadline {
        waiter_cx.cancel.cancelled().await;
    } else {
        waiter_cx.cancel.cancel();
    }

    match tokio::time::timeout(Duration::from_secs(1), waiter).await {
        Ok(joined) => {
            let err = joined.expect("waiter task").expect_err("waiter error");
            assert!(
                err.is_cancelled(),
                "{name}: waiter error={err}, want context cancellation"
            );
        }
        Err(_) => {
            panic!("{name}: waiter did not return while the connection owner remained blocked")
        }
    }
    assert_eq!(
        transport.connects(),
        1,
        "{name}: canceled waiter started an extra connection"
    );
    assert!(
        !owner.is_finished(),
        "{name}: canceling waiter interrupted owner"
    );
    transport.open_gate();

    (&mut owner)
        .await
        .expect("owner task")
        .unwrap_or_else(|err| panic!("{name}: owner failed: {err}"));
    let owner_session = c
        .session_peer()
        .unwrap_or_else(|| panic!("{name}: waiter destroyed the owner session"));

    // A canceled call must not disturb the cached session, even when the lock is free,
    // including through the refresh path's ping/close sequence.
    let err = acquire(c.clone(), refresh, waiter_cx)
        .await
        .expect_err("already canceled call");
    assert!(
        err.is_cancelled(),
        "{name}: already canceled call error={err}"
    );
    let session = c
        .get_session(&CallContext::background())
        .await
        .unwrap_or_else(|err| panic!("{name}: {err}"));
    assert!(
        same_session(&session, &owner_session),
        "{name}: session replaced"
    );
    ping(&session)
        .await
        .unwrap_or_else(|err| panic!("{name}: owner session no longer usable: {err}"));
    assert_eq!(
        transport.connects(),
        1,
        "{name}: session unexpectedly replaced"
    );

    c.close().await.expect("failed to close owner session");
}

// parity: adapted: Go's deadline subtests end the waiter with context.DeadlineExceeded; a Rust CallContext has only a CancellationToken, so a timer cancels it and the error is Error::Cancelled ("context canceled")
#[tokio::test]
async fn test_session_wait_cancellation() {
    for refresh in [false, true] {
        for deadline in [false, true] {
            session_wait_cancellation(refresh, deadline).await;
        }
    }
}

#[tokio::test]
async fn test_session_initialization_failure_recovery() {
    let (transport, c) = gated();
    let cx = CallContext::background();
    let first = tokio::spawn(acquire(c.clone(), false, cx.clone()));
    transport.entered().await;
    cx.cancel.cancel();

    let err = first
        .await
        .expect("first task")
        .expect_err("first connection");
    assert!(err.is_cancelled(), "first connection error={err}");
    assert_eq!(
        err.to_string(),
        "failed to init MCP session: context canceled"
    );

    let mut waiters = JoinSet::new();
    for _ in 0..8 {
        waiters.spawn(acquire(c.clone(), false, CallContext::background()));
    }

    transport.entered().await;
    assert_eq!(
        transport.connects(),
        2,
        "recovery must have one connection owner"
    );
    transport.open_gate();
    let mut sessions = Vec::new();
    while let Some(joined) = waiters.join_next().await {
        sessions.push(joined.expect("waiter task").expect("waiter"));
    }
    let current = c.session_peer().expect("recovered session");
    for session in &sessions {
        assert!(
            same_session(session, &current),
            "waiters did not share recovered session"
        );
    }
    assert_eq!(transport.connects(), 2, "recovery connected more than once");
    c.close().await.expect("failed to close recovered session");
}

/// Pins that close takes the session lock rather than racing a connection attempt. Without it,
/// close returns while the attempt is still running and the session that attempt then stores is
/// never closed.
#[tokio::test]
async fn test_close_waits_for_connection_in_progress() {
    let (transport, c) = gated();

    let owner = tokio::spawn(acquire(c.clone(), false, CallContext::background()));
    transport.entered().await;

    let mut close_done = tokio::spawn({
        let c = c.clone();
        async move { c.close().await }
    });
    if tokio::time::timeout(Duration::from_millis(100), &mut close_done)
        .await
        .is_ok()
    {
        transport.open_gate();
        let _ = owner.await;
        panic!("Close returned while a connection attempt was in progress");
    }

    transport.open_gate();
    let session = owner
        .await
        .expect("owner task")
        .unwrap_or_else(|err| panic!("getSession() error = {err}"));
    close_done
        .await
        .expect("close task")
        .unwrap_or_else(|err| panic!("Close() error = {err}"));
    assert!(
        ping(&session).await.is_err(),
        "session from the in-progress attempt is still open after Close"
    );
    let err = c
        .get_session(&CallContext::background())
        .await
        .expect_err("getSession() after Close");
    assert!(
        err.is_connection_closed(),
        "getSession() after Close error = {err}, want connection closed"
    );
}

/// Pins that lock fails on a done context even when the lock is free. The loop makes a missing
/// post-acquire check fail every run rather than by chance.
#[tokio::test]
async fn test_lock_fails_on_done_context() {
    let c = ConnectionRefresher::new(
        default_client_info(),
        ScriptedFactory::failing("no transport"),
    );
    let cx = CallContext::new(
        gadk_auth::AuthContext::anonymous(),
        CancellationToken::new(),
    );
    cx.cancel.cancel();
    for _ in 0..100 {
        match c.lock(&cx).await {
            Err(err) => assert!(
                err.is_cancelled(),
                "lock() error = {err}, want context canceled"
            ),
            Ok(_) => panic!("lock() succeeded on a done context"),
        }
    }
}
