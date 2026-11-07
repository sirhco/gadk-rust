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

//! The per-credential session pool (port of ADK-Go `tool/mcptoolset/client.go`
//! `credentialSessions`, `pooledSession`, `sessionKey`).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use gadk_auth::{AuthContext, CredentialProvider};
use http::{HeaderMap, HeaderName};
use rmcp::model::{CallToolRequestParams, CallToolResult, ClientConfig, Tool};
use sha2::{Digest, Sha256};
use tokio::sync::OnceCell;
use tokio::task::JoinSet;
use tokio::time::Instant;

use crate::client::{CallContext, ConnectionRefresher, McpClient, SessionClient};
use crate::config::TransportFactory;
use crate::error::Error;
use crate::transport::{HttpTarget, http_factory_for};

/// client.go `sessionIdleTimeout`: a session unused this long is closed on the next acquire.
pub(crate) const SESSION_IDLE_TIMEOUT: Duration = Duration::from_secs(600);

/// client.go `maxSessions`: the most sessions kept open while none are in use. Sessions in use
/// are never evicted, so the cap is exceeded only while more than that many are busy.
pub(crate) const MAX_SESSIONS: usize = 64;

/// A session key: the SHA-256 of the credential's headers ([`CredentialSessions::session_key`]).
pub(crate) type SessionKey = [u8; 32];

/// Builds the transport factory of a new session from the identity of the caller creating it.
type MakeFactory = Arc<dyn Fn(AuthContext) -> Arc<dyn TransportFactory> + Send + Sync>;

/// Go `pooledSession` as handed to a caller by [`CredentialSessions::acquire`]: one credential's
/// connection. Two handles are the same session when their `conn`s are the same `Arc`
/// ([`same`](PooledSession::same)). Its use count and last use live in the pool.
#[derive(Clone)]
pub(crate) struct PooledSession {
    /// The session key of the credential the session belongs to.
    pub(crate) key: SessionKey,
    /// The session's connection.
    pub(crate) conn: Arc<ConnectionRefresher>,
}

#[cfg(test)]
impl PooledSession {
    /// Go `==` on `*pooledSession`.
    pub(crate) fn same(&self, other: &PooledSession) -> bool {
        Arc::ptr_eq(&self.conn, &other.conn)
    }
}

/// A pooled session's use (Go `pooledSession.inUse`, `.lastUsed`), guarded by the pool mutex.
struct Entry {
    conn: Arc<ConnectionRefresher>,
    in_use: usize,
    last_used: Instant,
}

/// What the pool mutex guards (Go `credentialSessions.mu` and the fields under it, plus the
/// test-adjustable bounds, which Go keeps in package variables).
struct Pool {
    closed: bool,
    sessions: HashMap<SessionKey, Entry>,
    /// Go `evicting`: evicted sessions still closing, so `close` can wait for them.
    evicting: JoinSet<()>,
    idle_timeout: Duration,
    max_sessions: usize,
}

impl Pool {
    /// Go `evictIdleLocked`: closes sessions unused for the idle timeout.
    fn evict_idle(&mut self, now: Instant) {
        let idle_timeout = self.idle_timeout;
        let expired: Vec<SessionKey> = self
            .sessions
            .iter()
            .filter(|(_, s)| s.in_use == 0 && now.duration_since(s.last_used) >= idle_timeout)
            .map(|(key, _)| *key)
            .collect();
        for key in expired {
            self.evict(&key);
        }
    }

    /// Go `makeRoomLocked`: closes least recently used idle sessions until there is room for
    /// one more, or no idle session is left. A burst of concurrent callers can leave the pool
    /// over the cap, so this may close more than one.
    fn make_room(&mut self) {
        while self.sessions.len() >= self.max_sessions {
            let lru = self
                .sessions
                .iter()
                .filter(|(_, s)| s.in_use == 0)
                .min_by_key(|(_, s)| s.last_used)
                .map(|(key, _)| *key);
            let Some(key) = lru else {
                return;
            };
            self.evict(&key);
        }
    }

    /// Go `evictSessionLocked`: removes the session from the pool and closes it in the
    /// background, since closing sends a request to the server.
    fn evict(&mut self, key: &SessionKey) {
        let Some(entry) = self.sessions.remove(key) else {
            return;
        };
        // Reap evictions that have finished, so the set holds only those still closing.
        while self.evicting.try_join_next().is_some() {}
        // Without a runtime (a use dropped while its runtime shuts down) there is nothing to
        // close on; dropping the refresher drops its session.
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            self.evicting.spawn_on(
                async move {
                    // No caller is left to report a failure to, and the session is gone from
                    // the pool either way.
                    let _ = entry.conn.close().await;
                },
                &handle,
            );
        }
    }
}

/// Go `credentialSessions`: one [`ConnectionRefresher`] per distinct credential, keyed by
/// [`session_key`](CredentialSessions::session_key).
///
/// It is used when `Config::auth` is set. An MCP server that binds a session to the user who
/// created it rejects requests made on that session with another user's credential, so a
/// provider that mints a credential per user needs a session per user, while a
/// user-independent one such as `gadk_auth::static_token` serves every caller on one session.
/// Keying sessions on the credential the provider resolves covers both.
///
/// A session is closed once it has been idle for the idle timeout, or when the pool is full and
/// it is the least recently used idle one.
pub(crate) struct CredentialSessions {
    client_info: ClientConfig,
    provider: Arc<dyn CredentialProvider>,
    /// Each session's factory, bound to the identity of the caller that created it (Go
    /// `identityOnlyConnect`).
    make: MakeFactory,
    pool: Mutex<Pool>,
    /// Go `closeOnce` + `closeErr`.
    close_result: OnceCell<Result<(), Arc<Error>>>,
}

impl CredentialSessions {
    /// A pool whose sessions speak streamable HTTP to `target`, each bound to the identity of
    /// the caller that created it.
    pub(crate) fn new(
        client_info: ClientConfig,
        target: HttpTarget,
        provider: Arc<dyn CredentialProvider>,
    ) -> Self {
        let auth = Arc::clone(&provider);
        let make: MakeFactory =
            Arc::new(move |cx: AuthContext| http_factory_for(&target, Arc::clone(&auth), cx));
        Self::with_factory(client_info, provider, make)
    }

    /// A pool whose sessions connect through `make(identity)` (tests whose sessions do not speak
    /// HTTP, as `client_test.go`'s do not).
    pub(crate) fn with_factory(
        client_info: ClientConfig,
        provider: Arc<dyn CredentialProvider>,
        make: Arc<dyn Fn(AuthContext) -> Arc<dyn TransportFactory> + Send + Sync>,
    ) -> Self {
        Self {
            client_info,
            provider,
            make,
            pool: Mutex::new(Pool {
                closed: false,
                sessions: HashMap::new(),
                evicting: JoinSet::new(),
                idle_timeout: SESSION_IDLE_TIMEOUT,
                max_sessions: MAX_SESSIONS,
            }),
            close_result: OnceCell::new(),
        }
    }

    /// Go `sessionKey`: SHA-256 over the length-prefixed, sorted headers the credential resolved
    /// for `cx` applies.
    ///
    /// Every header name (lowercase, as `http::HeaderName` keeps it) is encoded as
    /// `{len(name)}:{name}{len(values)}:` followed by `{len(v)}:{v}` per value, so no two header
    /// sets encode alike. Errors are Go's `auth: resolve credential: …` and
    /// `auth: apply credential: …`; Go's `auth: provider returned nil credential` cannot occur
    /// (a provider returns an `Arc`).
    pub(crate) async fn session_key(&self, cx: &CallContext) -> Result<[u8; 32], Error> {
        let cred = self
            .provider
            .credential(&cx.auth)
            .await
            .map_err(Error::ResolveCredential)?;
        let mut headers = HeaderMap::new();
        cred.apply(&mut headers)
            .await
            .map_err(Error::ApplyCredential)?;
        let mut names: Vec<&HeaderName> = headers.keys().collect();
        names.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        let mut b = Vec::new();
        for name in names {
            let values: Vec<&[u8]> = headers.get_all(name).iter().map(|v| v.as_bytes()).collect();
            b.extend_from_slice(
                format!("{}:{}{}:", name.as_str().len(), name, values.len()).as_bytes(),
            );
            for v in values {
                b.extend_from_slice(format!("{}:", v.len()).as_bytes());
                b.extend_from_slice(v);
            }
        }
        Ok(Sha256::digest(&b).into())
    }

    fn lock_pool(&self) -> MutexGuard<'_, Pool> {
        self.pool.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Go `acquire`: the session for the credential of the caller in `cx`, created on first use
    /// and marked in use until [`release`](CredentialSessions::release). The session itself
    /// connects lazily.
    pub(crate) async fn acquire(&self, cx: &CallContext) -> Result<PooledSession, Error> {
        if self.lock_pool().closed {
            return Err(Error::ConnectionClosed);
        }
        let key = self.session_key(cx).await?;

        let mut pool = self.lock_pool();
        if pool.closed {
            return Err(Error::ConnectionClosed);
        }
        let now = Instant::now();
        // Sweep before the lookup so an expired session of this key is replaced rather than
        // handed out after it was evicted.
        pool.evict_idle(now);
        if let Some(entry) = pool.sessions.get_mut(&key) {
            entry.in_use += 1;
            entry.last_used = now;
            return Ok(PooledSession {
                key,
                conn: Arc::clone(&entry.conn),
            });
        }
        pool.make_room();
        let conn = Arc::new(ConnectionRefresher::new(
            self.client_info.clone(),
            (self.make)(cx.auth.clone()),
        ));
        pool.sessions.insert(
            key,
            Entry {
                conn: Arc::clone(&conn),
                in_use: 1,
                last_used: now,
            },
        );
        Ok(PooledSession { key, conn })
    }

    /// Go `release`: ends a use of `s` that finished with `err`. A session that never connected,
    /// or lost its connection and could not reconnect, is dropped so the pool does not keep
    /// entries for credentials the server refuses.
    pub(crate) fn release(&self, s: &PooledSession, err: Option<&Error>) {
        let mut pool = self.lock_pool();
        // A session no longer pooled (evicted or closed) has no use count left to update.
        let Some(entry) = pool.sessions.get_mut(&s.key) else {
            return;
        };
        if !Arc::ptr_eq(&entry.conn, &s.conn) {
            return;
        }
        entry.in_use = entry.in_use.saturating_sub(1);
        entry.last_used = Instant::now();
        // `disconnected` does not block, so checking it under the pool lock means the last
        // release of a failed session always sees it.
        if err.is_some() && entry.in_use == 0 && s.conn.disconnected() {
            pool.evict(&s.key);
        }
    }

    /// Replaces the idle timeout and the session cap (Go `SetSessionBoundsForTest`).
    #[cfg(test)]
    pub(crate) fn set_bounds(&self, idle_timeout: Duration, max_sessions: usize) {
        let mut pool = self.lock_pool();
        pool.idle_timeout = idle_timeout;
        pool.max_sessions = max_sessions;
    }

    /// How many sessions the pool keeps (Go `SessionCountForTest`).
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.lock_pool().sessions.len()
    }

    /// Whether `s` is the session the pool keeps for its key (Go `p.sessions[s.key] == s`).
    #[cfg(test)]
    pub(crate) fn is_pooled(&self, s: &PooledSession) -> bool {
        self.lock_pool()
            .sessions
            .get(&s.key)
            .is_some_and(|entry| Arc::ptr_eq(&entry.conn, &s.conn))
    }
}

/// One use of a pooled session: released when dropped, as Go's deferred `release`. A use whose
/// future is dropped before it finishes counts as failed (Go's operation would return the
/// context's error).
struct Use<'a> {
    pool: &'a CredentialSessions,
    session: PooledSession,
    failed: bool,
}

impl Drop for Use<'_> {
    fn drop(&mut self) {
        let err = self.failed.then_some(&Error::Cancelled);
        self.pool.release(&self.session, err);
    }
}

#[async_trait::async_trait]
impl McpClient for CredentialSessions {
    /// Go `CallTool`: `tools/call` on the session for the caller's credential.
    async fn call_tool(
        &self,
        cx: &CallContext,
        params: CallToolRequestParams,
    ) -> Result<CallToolResult, Error> {
        let mut u = Use {
            pool: self,
            session: self.acquire(cx).await?,
            failed: true,
        };
        let result = u.session.conn.call_tool(cx, params).await;
        u.failed = result.is_err();
        result
    }

    /// Go `ListTools`: lists tools on the session for the caller's credential.
    async fn list_tools(&self, cx: &CallContext) -> Result<Vec<Tool>, Error> {
        let mut u = Use {
            pool: self,
            session: self.acquire(cx).await?,
            failed: true,
        };
        let result = u.session.conn.list_tools(cx).await;
        u.failed = result.is_err();
        result
    }
}

/// An `Arc<Error>` shared by a closed refresher as an owned [`Error`]: the error itself when no
/// one else holds it, else wrapped with its text unchanged.
fn unshare(err: Arc<Error>) -> Error {
    Arc::try_unwrap(err).unwrap_or_else(|shared| Error::Transport(Box::new(shared)))
}

#[async_trait::async_trait]
impl SessionClient for CredentialSessions {
    /// Go `Close`: closes every session, waiting for connection setup in progress as
    /// [`ConnectionRefresher`]'s `close` does, then for evicted sessions still closing.
    /// Idempotent and concurrency-safe: every call returns the first call's result. Later
    /// operations fail with [`Error::ConnectionClosed`].
    async fn close(&self) -> Result<(), Arc<Error>> {
        self.close_result
            .get_or_init(|| async {
                let (sessions, mut evicting) = {
                    let mut pool = self.lock_pool();
                    pool.closed = true;
                    (
                        std::mem::take(&mut pool.sessions),
                        std::mem::take(&mut pool.evicting),
                    )
                };
                let mut errs = Vec::new();
                for (_, entry) in sessions {
                    if let Err(err) = entry.conn.close().await {
                        // Drop the refresher first, so the error is ours alone when we can.
                        drop(entry);
                        errs.push(unshare(err));
                    }
                }
                while evicting.join_next().await.is_some() {}
                if errs.is_empty() {
                    Ok(())
                } else {
                    Err(Arc::new(Error::Close(errs)))
                }
            })
            .await
            .clone()
    }

    fn as_credential_sessions(&self) -> Option<&CredentialSessions> {
        Some(self)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::sync::Arc;

    use super::CredentialSessions;
    use crate::client::{CallContext, McpClient, SessionClient};
    use crate::config::{TransportFactory, default_client_info};
    use crate::error::Error;
    use crate::testutil::{ScriptedFactory, ToolServer};
    use gadk_auth::{AuthContext, Bearer, Credential, CredentialFuture, CredentialProvider};

    /// A credential setting fixed headers, in order (a name may repeat).
    #[derive(Debug)]
    struct Headers(Vec<(&'static str, &'static str)>);

    #[async_trait::async_trait]
    impl Credential for Headers {
        async fn apply(&self, headers: &mut http::HeaderMap) -> Result<(), gadk_auth::Error> {
            for (name, value) in &self.0 {
                headers.append(*name, http::HeaderValue::from_static(value));
            }
            Ok(())
        }
    }

    /// A credential whose `apply` fails.
    #[derive(Debug)]
    struct Unappliable;

    #[async_trait::async_trait]
    impl Credential for Unappliable {
        async fn apply(&self, _: &mut http::HeaderMap) -> Result<(), gadk_auth::Error> {
            Err(gadk_auth::Error::msg("no headers"))
        }
    }

    fn pool(provider: Arc<dyn CredentialProvider>) -> CredentialSessions {
        let factory: Arc<dyn TransportFactory> = ScriptedFactory::new(|| ToolServer::new("t", "1"));
        CredentialSessions::with_factory(
            default_client_info(),
            provider,
            Arc::new(move |_| Arc::clone(&factory)),
        )
    }

    fn fixed(cred: Arc<dyn Credential>) -> Arc<dyn CredentialProvider> {
        gadk_auth::ProviderFn::new(move |_: &AuthContext| -> CredentialFuture {
            let cred = Arc::clone(&cred);
            Box::pin(async move { Ok(cred) })
        })
        .into_provider()
    }

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    /// The key is SHA-256 of Go's length-prefixed encoding over the sorted, lowercase names:
    /// `authorization` sorts before `x-key`, and a repeated name counts its values.
    #[tokio::test]
    async fn session_key_hashes_the_length_prefixed_sorted_headers() {
        let p = pool(fixed(Arc::new(Headers(vec![
            ("x-key", "a"),
            ("x-key", "b"),
            ("authorization", "Bearer tok"),
        ]))));
        let key = p.session_key(&CallContext::background()).await.unwrap();
        // python3 -c 'import hashlib;print(hashlib.sha256(b"13:authorization1:10:Bearer tok5:x-key2:1:a1:b").hexdigest())'
        assert_eq!(
            hex(&key),
            "d9e95cf300f5d0ea355c9eb4ca1c3d213316a909475280c92150e37486761f50"
        );
        let bearer = pool(fixed(Arc::new(Bearer {
            token: "tok".into(),
        })));
        let key = bearer
            .session_key(&CallContext::background())
            .await
            .unwrap();
        // python3 -c 'import hashlib;print(hashlib.sha256(b"13:authorization1:10:Bearer tok").hexdigest())'
        assert_eq!(
            hex(&key),
            "441eef59307dba3d96411653b3d294c3fbfb8e1335d4d489565c9ef3e66f96a4"
        );
    }

    #[tokio::test]
    async fn session_key_errors_carry_go_texts() {
        let failing = gadk_auth::ProviderFn::new(|_: &AuthContext| -> CredentialFuture {
            Box::pin(async { Err(gadk_auth::Error::msg("no token")) })
        })
        .into_provider();
        let err = pool(failing)
            .session_key(&CallContext::background())
            .await
            .unwrap_err();
        assert!(matches!(err, Error::ResolveCredential(_)));
        assert_eq!(err.to_string(), "auth: resolve credential: no token");

        let err = pool(fixed(Arc::new(Unappliable)))
            .session_key(&CallContext::background())
            .await
            .unwrap_err();
        assert!(matches!(err, Error::ApplyCredential(_)));
        assert_eq!(err.to_string(), "auth: apply credential: no headers");
    }

    /// Go `release`: a session whose use failed and that never connected leaves the pool.
    #[tokio::test]
    async fn a_failed_session_is_dropped_on_release() {
        let factory: Arc<dyn TransportFactory> = ScriptedFactory::failing("not used");
        let p = CredentialSessions::with_factory(
            default_client_info(),
            fixed(Arc::new(Bearer {
                token: "tok".into(),
            })),
            Arc::new(move |_| Arc::clone(&factory)),
        );
        let err = p.list_tools(&CallContext::background()).await.unwrap_err();
        assert_eq!(
            err.to_string(),
            "failed to list MCP tools: failed to init MCP session: not used"
        );
        assert_eq!(p.len(), 0);
        p.close().await.unwrap();
    }

    /// A use whose future is dropped mid-connect is released as failed (Go's deferred `release`
    /// with the context's error), so the never-connected session leaves the pool.
    #[tokio::test]
    async fn a_dropped_use_is_released() {
        let factory = ScriptedFactory::new(|| ToolServer::new("t", "1"));
        factory.close_gate();
        let shared: Arc<dyn TransportFactory> = factory.clone();
        let p = Arc::new(CredentialSessions::with_factory(
            default_client_info(),
            fixed(Arc::new(Bearer {
                token: "tok".into(),
            })),
            Arc::new(move |_| Arc::clone(&shared)),
        ));
        let task = tokio::spawn({
            let p = Arc::clone(&p);
            async move { p.list_tools(&CallContext::background()).await }
        });
        factory.entered().await;
        assert_eq!(p.len(), 1);
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert_eq!(p.len(), 0);
        p.close().await.unwrap();
    }

    /// A new pool is empty and starts at Go's bounds.
    #[test]
    fn new_pool_starts_empty_at_the_default_bounds() {
        let p = pool(fixed(Arc::new(Bearer::default())));
        let pool = p.lock_pool();
        assert!(pool.sessions.is_empty());
        assert_eq!(pool.idle_timeout, super::SESSION_IDLE_TIMEOUT);
        assert_eq!(pool.max_sessions, super::MAX_SESSIONS);
    }
}
