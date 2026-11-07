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

//! Port of Go `tool/mcptoolset/client_test.go` (package `mcptoolset`; Task 8).
//!
//! Go's `connectFunc` over an in-memory server is [`ScriptedFactory::new`]; the pool's
//! never-connecting transport is [`ScriptedFactory::failing`]. Go's `SetSessionBoundsForTest` is
//! [`CredentialSessions::set_bounds`], and `p.sessions` is read through
//! [`CredentialSessions::len`] and [`CredentialSessions::is_pooled`].

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use gadk_auth::{AuthContext, Bearer, CredentialFuture, CredentialProvider, Identity, ProviderFn};
use tokio_util::sync::CancellationToken;

use crate::client::{CallContext, ConnectionRefresher, McpClient, SessionClient};
use crate::config::{TransportFactory, default_client_info};
use crate::error::Error;
use crate::sessions::{CredentialSessions, PooledSession};
use crate::testutil::{ScriptedFactory, ToolServer};

#[tokio::test]
async fn test_close_prevents_retry() {
    let factory = ScriptedFactory::new(|| ToolServer::new("test_server", "1"));
    let client = Arc::new(ConnectionRefresher::new(
        default_client_info(),
        factory.clone(),
    ));
    let calls = Arc::new(AtomicUsize::new(0));
    let err = client
        .with_retry(&CallContext::background(), |_peer| {
            let (client, calls) = (Arc::clone(&client), Arc::clone(&calls));
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                // The toolset closes after an operation starts, before its error is handled.
                if let Err(err) = client.close().await {
                    panic!("close: {err}");
                }
                Err::<(), _>(Error::ConnectionClosed)
            }
        })
        .await
        .unwrap_err();
    assert!(
        err.is_connection_closed(),
        "with_retry() error = {err}, want ErrConnectionClosed"
    );
    let (calls, connects) = (calls.load(Ordering::SeqCst), factory.connects());
    assert!(
        calls == 1 && connects == 1,
        "after close(): {calls} calls and {connects} connections, want 1 each"
    );
    client.close().await.unwrap();
}

/// Go `testUserKey`: the caller's user, as its invocation identity.
fn user(name: &str) -> CallContext {
    CallContext::new(
        AuthContext::for_identity(Identity {
            user_id: name.to_owned(),
            ..Identity::default()
        }),
        CancellationToken::new(),
    )
}

/// Go `newTestCredentialSessions`'s provider: a bearer token naming the caller's user
/// (`anonymous` without an identity, since an empty bearer token cannot be applied).
fn user_provider() -> Arc<dyn CredentialProvider> {
    ProviderFn::new(|cx: &AuthContext| -> CredentialFuture {
        let token = cx
            .identity
            .as_ref()
            .map_or_else(|| "anonymous".to_owned(), |id| id.user_id.clone());
        Box::pin(async move { Ok(Arc::new(Bearer { token }) as _) })
    })
    .into_provider()
}

/// Go `newTestCredentialSessions`: a pool keyed on the caller's user, whose sessions never
/// connect.
fn new_test_credential_sessions() -> CredentialSessions {
    let factory: Arc<dyn TransportFactory> = ScriptedFactory::failing("not used");
    CredentialSessions::with_factory(
        default_client_info(),
        user_provider(),
        Arc::new(move |_| Arc::clone(&factory)),
    )
}

/// Checks that a credential whose own session went idle past the timeout gets a new session, not
/// the evicted one.
#[tokio::test(start_paused = true)]
async fn test_credential_sessions_replace_own_idle_session() {
    let p = new_test_credential_sessions();
    p.set_bounds(Duration::from_millis(10), 64);
    let cx = user("alice");

    let first = p.acquire(&cx).await.expect("acquire");
    p.release(&first, None);
    tokio::time::sleep(Duration::from_millis(20)).await;

    let second = p.acquire(&cx).await.expect("acquire after idle timeout");
    p.release(&second, None);
    assert!(
        !second.same(&first),
        "acquire after idle timeout returned the evicted session"
    );
    assert!(
        p.is_pooled(&second),
        "session returned after idle timeout is not in the pool"
    );
    p.close().await.unwrap();
}

/// Checks that once a burst of concurrent users has pushed the pool over the cap, the next new
/// credential evicts idle sessions until the pool is back within the cap.
#[tokio::test]
async fn test_credential_sessions_shrink_to_cap_after_burst() {
    let p = new_test_credential_sessions();
    p.set_bounds(Duration::from_secs(3600), 2);

    let mut held: Vec<PooledSession> = Vec::new();
    for name in ["alice", "bob", "carol"] {
        let s = p
            .acquire(&user(name))
            .await
            .unwrap_or_else(|err| panic!("acquire({name}): {err}"));
        held.push(s);
    }
    for s in &held {
        p.release(s, None);
    }

    let s = p.acquire(&user("dave")).await.expect("acquire(dave)");
    p.release(&s, None);
    assert_eq!(p.len(), 2, "pool keeps {} sessions, want 2", p.len());
    p.close().await.unwrap();
}

/// A pool whose session factories record the identity each was built for and serve a fresh
/// in-memory server.
fn recording_pool() -> (CredentialSessions, Arc<Mutex<Vec<AuthContext>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let record = Arc::clone(&seen);
    let p = CredentialSessions::with_factory(
        default_client_info(),
        user_provider(),
        Arc::new(move |auth: AuthContext| -> Arc<dyn TransportFactory> {
            record
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(auth);
            ScriptedFactory::new(|| ToolServer::new("test_server", "1"))
        }),
    );
    (p, seen)
}

fn seen(seen: &Mutex<Vec<AuthContext>>) -> Vec<AuthContext> {
    seen.lock().unwrap_or_else(PoisonError::into_inner).clone()
}

// parity: adapted: Go connects a pooled session on identityOnlyContext(ctx), a context carrying only the caller's ADK identity and its cancellation; Rust passes an explicit AuthContext, so the check is that the session's factory is built from exactly the creating caller's AuthContext (its identity and nothing else), once per session, while cancellation stays on each operation's CallContext.
#[tokio::test]
async fn test_identity_only_context() {
    let want = Identity {
        app_name: "app".to_owned(),
        user_id: "alice".to_owned(),
        session_id: String::new(),
    };
    let (p, recorded) = recording_pool();
    let cx = CallContext::new(
        AuthContext::for_identity(want.clone()),
        CancellationToken::new(),
    );

    let tools = p.list_tools(&cx).await.expect("list_tools");
    assert!(tools.is_empty(), "list_tools = {tools:?}, want none");
    // A second call with the same credential reuses the session: no new factory.
    p.list_tools(&cx).await.expect("second list_tools");

    assert_eq!(
        seen(&recorded),
        vec![AuthContext::for_identity(want)],
        "session factory identities"
    );
    assert_eq!(p.len(), 1);
    p.close().await.unwrap();
}

// parity: adapted: Go's identityOnlyContext without an ADK identity yields a context without one, which stop cancels; Rust's anonymous caller's session factory gets AuthContext::anonymous(), and close() ends the session.
#[tokio::test]
async fn test_identity_only_context_without_identity() {
    let (p, recorded) = recording_pool();

    p.list_tools(&CallContext::background())
        .await
        .expect("list_tools");

    assert_eq!(
        seen(&recorded),
        vec![AuthContext::anonymous()],
        "session factory identities"
    );
    p.close().await.unwrap();
    let err = p.list_tools(&CallContext::background()).await.unwrap_err();
    assert!(err.is_connection_closed(), "after close: {err}");
}
