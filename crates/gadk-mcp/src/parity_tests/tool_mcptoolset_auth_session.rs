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

//! Port of Go `tool/mcptoolset/auth_session_test.go` (package `mcptoolset_test`; Task 10).
//!
//! Go's `userBoundServer` is [`HttpTestServer::user_bound`]: `Bearer token-of-<user>` is required,
//! each MCP session is bound to the user that created it (403 `session user mismatch` for anyone
//! else), and `whoami` answers the caller. Go's `invocationAs` builds a [`Context`] over an
//! in-memory session of `user` in `app`; the pool reads its identity through
//! `CallContext::from(&ReadonlyContext)`. Go's `SetSessionBoundsForTest`/`SessionCountForTest` are
//! [`McpToolset::set_session_bounds_for_test`]/[`McpToolset::session_count_for_test`].
//!
//! The pool's clock is `tokio::time::Instant` (D6), so the idle-eviction tests sleep past the
//! shrunk timeout with `tokio::time::sleep` on the real clock: the server is real loopback HTTP,
//! and a paused clock would auto-advance into rmcp's timers while the runtime waits on sockets.

use std::sync::{Arc, Weak};
use std::time::Duration;

use gadk_agent::tool::{Tool, Toolset};
use gadk_agent::{Context, ContextDelta, ReadonlyContext, ToolError};
use gadk_auth::{AuthContext, Bearer, CredentialFuture, CredentialProvider, ProviderFn};
use gadk_session::{CreateRequest, InMemorySessionService, Session, SessionService};
use serde_json::json;

use crate::client::{CallContext, McpClient, SessionClient};
use crate::config::{Config, McpTransport, default_client_info};
use crate::error::Error;
use crate::sessions::CredentialSessions;
use crate::set::McpToolset;
use crate::testutil::HttpTestServer;
use crate::transport::HttpTarget;

/// Go `perUserToken`: mints `token-of-<user>` for the acting user.
fn per_user_token() -> Arc<dyn CredentialProvider> {
    ProviderFn::new(|cx: &AuthContext| -> CredentialFuture {
        let user = cx
            .identity
            .as_ref()
            .map(|id| id.user_id.clone())
            .unwrap_or_default();
        Box::pin(async move {
            if user.is_empty() {
                return Err(gadk_auth::Error::msg("no acting user"));
            }
            Ok(Arc::new(Bearer {
                token: format!("token-of-{user}"),
            }) as _)
        })
    })
    .into_provider()
}

/// Go `newAuthToolset`: a toolset on `server` whose credentials come from `provider`. Go closes it
/// in `t.Cleanup`; the tests here close it at their end.
fn new_auth_toolset(server: &HttpTestServer, provider: Arc<dyn CredentialProvider>) -> McpToolset {
    let mut cfg = Config::new(McpTransport::Endpoint(server.url()));
    cfg.auth = Some(provider);
    crate::new(cfg).unwrap_or_else(|e| panic!("mcptoolset.New() failed: {e}"))
}

/// A session of `user` in `app` from a fresh in-memory session service.
async fn session_of(app: &str, user: &str) -> Arc<dyn Session> {
    InMemorySessionService::new()
        .create(CreateRequest {
            app_name: app.into(),
            user_id: user.into(),
            ..Default::default()
        })
        .await
        .unwrap_or_else(|e| panic!("Create session for {user:?}: {e}"))
}

/// Go `invocationAs`: an invocation context whose session belongs to `user` of `app`.
async fn invocation_as(app: &str, user: &str) -> Context {
    Context::builder()
        .session(session_of(app, user).await)
        .build()
}

/// Go `agent.NewToolContext(invCtx, "", nil, nil)`.
fn tool_context(ctx: &Context) -> Context {
    ctx.with_delta(ContextDelta {
        function_call_id: Some(String::new()),
        actions: Some(Default::default()),
        ..Default::default()
    })
}

/// Go `callWhoami`: lists the toolset's tools and runs `whoami` in `ctx`, returning the user the
/// server saw.
async fn call_whoami(ts: &McpToolset, ctx: &Context) -> Result<String, String> {
    let tools = ts.tools(ctx).await.map_err(|e| format!("Tools(): {e}"))?;
    if tools.len() != 1 {
        return Err(format!("Tools() returned {} tools, want 1", tools.len()));
    }
    let runnable = tools[0]
        .as_runnable()
        .ok_or_else(|| "whoami is not runnable".to_string())?;
    let got = runnable
        .run(tool_context(ctx), json!({}))
        .await
        .map_err(|e| format!("Run(): {e}"))?;
    // Go fmt.Sprint(got["output"]) of the text content.
    Ok(match got.get("output") {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
        None => "<nil>".to_string(),
    })
}

/// Go `callAs`.
async fn call_as(ts: &McpToolset, app: &str, user: &str, want: &str) {
    let got = call_whoami(ts, &invocation_as(app, user).await)
        .await
        .unwrap_or_else(|e| panic!("as {app}/{user}: {e}"));
    assert_eq!(
        got, want,
        "as {app}/{user}: server saw {got:?}, want {want:?}"
    );
}

/// The crate error behind a `ToolError::Execution`.
fn execution_source(err: &ToolError) -> &Error {
    match err {
        ToolError::Execution(source) => source
            .downcast_ref::<Error>()
            .unwrap_or_else(|| panic!("not a gadk_mcp::Error: {source}")),
        other => panic!("want ToolError::Execution, got {other:?}"),
    }
}

#[tokio::test]
async fn test_auth_gives_each_user_its_own_session() {
    let server = HttpTestServer::user_bound().await;
    let ts = new_auth_toolset(&server, per_user_token());

    for user in ["alice", "bob", "alice", "bob"] {
        call_as(&ts, "app", user, user).await;
    }

    // Each user stays on one session: bob's calls did not tear down alice's.
    for user in ["alice", "bob"] {
        let n = server.sessions_of(user);
        assert_eq!(n, 1, "{user} used {n} MCP sessions, want 1");
    }
    ts.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_auth_concurrent_users() {
    let server = HttpTestServer::user_bound().await;
    let ts = Arc::new(new_auth_toolset(&server, per_user_token()));

    const USERS: usize = 8;
    const CALLS_PER_USER: usize = 4;
    let mut calls = tokio::task::JoinSet::new();
    for u in 0..USERS {
        let user = format!("user{u}");
        for _ in 0..CALLS_PER_USER {
            let ctx = invocation_as("app", &user).await;
            let (ts, user) = (Arc::clone(&ts), user.clone());
            calls.spawn(async move {
                match call_whoami(&ts, &ctx).await {
                    Ok(got) if got == user => Ok(()),
                    Ok(got) => Err(format!("as {user}: server saw {got:?}, want {user:?}")),
                    Err(e) => Err(format!("as {user}: {e}")),
                }
            });
        }
    }
    let mut errs = Vec::new();
    while let Some(joined) = calls.join_next().await {
        if let Err(e) = joined.unwrap() {
            errs.push(e);
        }
    }
    assert!(errs.is_empty(), "{errs:#?}");
    for u in 0..USERS {
        let n = server.sessions_of(&format!("user{u}"));
        assert_eq!(n, 1, "user{u} used {n} MCP sessions, want 1");
    }
    ts.close().await.unwrap();
}

#[tokio::test]
async fn test_auth_same_user_across_apps() {
    let server = HttpTestServer::user_bound().await;
    let per_app_token = ProviderFn::new(|cx: &AuthContext| -> CredentialFuture {
        let id = cx.identity.clone().unwrap_or_default();
        Box::pin(async move {
            Ok(Arc::new(Bearer {
                token: format!("token-of-{}.{}", id.app_name, id.user_id),
            }) as _)
        })
    })
    .into_provider();
    let ts = new_auth_toolset(&server, per_app_token);

    for app in ["app1", "app2", "app1", "app2"] {
        call_as(&ts, app, "alice", &format!("{app}.alice")).await;
    }
    for user in ["app1.alice", "app2.alice"] {
        let n = server.sessions_of(user);
        assert_eq!(n, 1, "{user} used {n} MCP sessions, want 1");
    }
    ts.close().await.unwrap();
}

#[tokio::test]
async fn test_auth_user_independent_credential_shares_one_session() {
    let server = HttpTestServer::user_bound().await;
    let ts = new_auth_toolset(&server, gadk_auth::static_token("token-of-svc"));

    for user in ["alice", "bob", "carol"] {
        call_as(&ts, "app", user, "svc").await;
    }
    let n = server.sessions_of("svc");
    assert_eq!(
        n, 1,
        "three users with one static token used {n} MCP sessions, want 1"
    );
    ts.close().await.unwrap();
}

// parity: adapted: a ReadonlyContext always carries an identity, so Go's caller without one is CallContext::background() on the pool new() builds for Config.auth
#[tokio::test]
async fn test_auth_without_acting_user_shares_one_session() {
    let server = HttpTestServer::user_bound().await;
    let target = HttpTarget::from_transport(&McpTransport::Endpoint(server.url()))
        .unwrap_or_else(|| panic!("an endpoint is an HTTP target"));
    let pool = CredentialSessions::new(
        default_client_info(),
        target,
        gadk_auth::static_token("token-of-svc"),
    );

    for _ in 0..2 {
        let cx = CallContext::background();
        assert_eq!(cx.auth, AuthContext::anonymous());
        if let Err(e) = pool.list_tools(&cx).await {
            panic!("Tools() failed: {e}");
        }
    }
    let n = server.sessions_of("svc");
    assert_eq!(n, 1, "callers without a user used {n} MCP sessions, want 1");
    pool.close().await.unwrap();
}

#[tokio::test]
async fn test_auth_evicts_idle_sessions() {
    let server = HttpTestServer::user_bound().await;
    let ts = new_auth_toolset(&server, per_user_token());
    ts.set_session_bounds_for_test(Duration::from_millis(50), 64);

    call_as(&ts, "app", "alice", "alice").await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    call_as(&ts, "app", "bob", "bob").await; // evicts alice's idle session
    call_as(&ts, "app", "alice", "alice").await;

    if let Err(e) = ts.close().await {
        panic!("Close() failed: {e}");
    }
    let n = server.sessions_of("alice");
    assert_eq!(
        n, 2,
        "alice used {n} MCP sessions, want 2 (one evicted, one new)"
    );
    let n = server.deleted_of("alice");
    assert_eq!(n, 2, "alice's sessions closed with DELETE: {n}, want 2");
}

#[tokio::test]
async fn test_auth_idle_session_same_user_returns() {
    let server = HttpTestServer::user_bound().await;
    let ts = new_auth_toolset(&server, per_user_token());
    ts.set_session_bounds_for_test(Duration::from_millis(50), 64);

    call_as(&ts, "app", "alice", "alice").await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    call_as(&ts, "app", "alice", "alice").await;

    let n = ts.session_count_for_test();
    assert_eq!(n, 1, "toolset keeps {n} sessions, want 1");
    let n = server.sessions_of("alice");
    assert_eq!(
        n, 2,
        "alice used {n} MCP sessions, want 2 (one evicted, one new)"
    );
    ts.close().await.unwrap();
}

#[tokio::test]
async fn test_auth_caps_open_sessions() {
    let server = HttpTestServer::user_bound().await;
    let ts = new_auth_toolset(&server, per_user_token());
    ts.set_session_bounds_for_test(Duration::from_secs(3600), 2);

    for user in ["alice", "bob", "carol"] {
        call_as(&ts, "app", user, user).await;
    }
    let n = ts.session_count_for_test();
    assert_eq!(n, 2, "toolset keeps {n} sessions, want 2");
    call_as(&ts, "app", "alice", "alice").await;
    let n = server.sessions_of("alice");
    assert_eq!(
        n, 2,
        "alice used {n} MCP sessions, want 2 (least recently used one evicted)"
    );
    let n = server.sessions_of("carol");
    assert_eq!(n, 1, "carol used {n} MCP sessions, want 1");
    ts.close().await.unwrap();
}

#[tokio::test]
async fn test_auth_drops_session_that_failed_to_connect() {
    let server = HttpTestServer::user_bound().await;
    let rejected = ProviderFn::new(|cx: &AuthContext| -> CredentialFuture {
        let user = cx
            .identity
            .as_ref()
            .map(|id| id.user_id.clone())
            .unwrap_or_default();
        Box::pin(async move {
            Ok(Arc::new(Bearer {
                token: format!("revoked-{user}"),
            }) as _)
        })
    })
    .into_provider();
    let ts = new_auth_toolset(&server, rejected);

    for user in ["alice", "bob"] {
        let ctx = invocation_as("app", user).await;
        if let Ok(tools) = ts.tools(&ctx).await {
            panic!(
                "Tools() as {user} with a refused credential succeeded with {} tools, want error",
                tools.len()
            );
        }
    }
    let n = ts.session_count_for_test();
    assert_eq!(
        n, 0,
        "toolset keeps {n} sessions after failed connects, want 0"
    );
    ts.close().await.unwrap();
}

// parity: adapted: no GC or context values in Rust; a Weak to the invocation's session is dead once its Context is dropped, so the open MCP session keeps nothing of the invocation (P5)
#[tokio::test]
async fn test_auth_does_not_retain_invocation() {
    let server = HttpTestServer::user_bound().await;
    let ts = new_auth_toolset(&server, per_user_token());

    let weak: Weak<dyn Session> = {
        // The service is dropped with this block, so the context holds the only strong refs.
        let session = session_of("app", "alice").await;
        let weak = Arc::downgrade(&session);
        let ctx = Context::builder().session(session).build();
        let tools: Vec<Arc<dyn Tool>> = ts
            .tools(&ctx)
            .await
            .unwrap_or_else(|e| panic!("Tools() failed: {e}"));
        assert_eq!(tools.len(), 1);
        weak
    };

    assert_eq!(ts.session_count_for_test(), 1, "the MCP session stays open");
    assert!(
        weak.upgrade().is_none(),
        "the open MCP session keeps the invocation that opened it in memory"
    );
    ts.close().await.unwrap();
}

#[tokio::test]
async fn test_auth_close_closes_every_users_session() {
    let server = HttpTestServer::user_bound().await;
    let ts = new_auth_toolset(&server, per_user_token());

    let (alice, bob) = (
        invocation_as("app", "alice").await,
        invocation_as("app", "bob").await,
    );
    let mut tools: Vec<Arc<dyn Tool>> = Vec::new();
    for ctx in [&alice, &bob] {
        tools = ts
            .tools(ctx)
            .await
            .unwrap_or_else(|e| panic!("Tools() failed: {e}"));
    }

    if let Err(e) = ts.close().await {
        panic!("Close() failed: {e}");
    }
    for user in ["alice", "bob"] {
        let n = server.deleted_of(user);
        assert_eq!(n, 1, "{user}'s sessions closed with DELETE: {n}, want 1");
    }
    for ctx in [&alice, &bob] {
        let readonly: &ReadonlyContext = ctx;
        match ts.tools(readonly).await {
            Ok(tools) => panic!(
                "Tools() after Close() listed {} tools, want connection closed",
                tools.len()
            ),
            Err(err) => assert!(
                execution_source(&err).is_connection_closed(),
                "Tools() after Close() error = {err}, want connection closed"
            ),
        }
    }
    let runnable = tools[0]
        .as_runnable()
        .unwrap_or_else(|| panic!("whoami is not runnable"));
    match runnable.run(tool_context(&alice), json!({})).await {
        Ok(got) => panic!("Run() after Close() = {got:?}, want connection closed"),
        Err(err) => assert!(
            execution_source(&err).is_connection_closed(),
            "Run() after Close() error = {err}, want connection closed"
        ),
    }
}

#[tokio::test]
async fn test_auth_concurrent_close_waits() {
    let server = HttpTestServer::user_bound().await;
    // Go's front server: hold the slow user's connection setup open.
    let hold = server.hold_initialize("token-of-slow");
    let ts = Arc::new(new_auth_toolset(&server, per_user_token()));

    let ctx = invocation_as("app", "slow").await;
    let slow = tokio::spawn({
        let ts = Arc::clone(&ts);
        async move {
            let _ = ts.tools(&ctx).await;
        }
    });
    hold.initializing().await;

    let first = tokio::spawn({
        let ts = Arc::clone(&ts);
        async move {
            let _ = ts.close().await;
        }
    });
    tokio::time::sleep(Duration::from_millis(100)).await; // let the first close start waiting
    let mut second = tokio::spawn({
        let ts = Arc::clone(&ts);
        async move {
            let _ = ts.close().await;
        }
    });

    tokio::select! {
        _ = &mut second => panic!("second Close returned while the first was still closing sessions"),
        () = tokio::time::sleep(Duration::from_millis(100)) => {}
    }
    hold.release();
    first.await.unwrap();
    second.await.unwrap();
    slow.await.unwrap();
}
