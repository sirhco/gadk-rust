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

//! In-memory [`TransportFactory`]s: [`in_memory`] and the observable [`ScriptedFactory`], which
//! counts connects, can gate or fail them, and can end the last connection's server side.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use futures::future::BoxFuture;
use rmcp::model::ClientConfig;
use rmcp::service::RunningService;
use rmcp::{RoleClient, ServerHandler, ServiceExt};
use tokio::io::DuplexStream;
use tokio::sync::{Semaphore, watch};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::lock;
use crate::config::{TransportFactory, serve_client};
use crate::error::BoxError;

/// Buffer of each in-memory pipe direction.
const DUPLEX_BUFFER: usize = 64 * 1024;

/// Serves a fresh server on the server half of a pipe until the client half closes or the token
/// is cancelled.
type ServeFn = Arc<dyn Fn(DuplexStream, CancellationToken) -> BoxFuture<'static, ()> + Send + Sync>;

fn serve_fn<S, F>(make_server: F) -> ServeFn
where
    F: Fn() -> S + Send + Sync + 'static,
    S: ServerHandler,
{
    Arc::new(move |io, ct| {
        let server = make_server();
        Box::pin(async move {
            if let Ok(running) = server.serve_with_ct(io, ct).await {
                let _ = running.waiting().await;
            }
        })
    })
}

/// Each connect spawns `make_server()` on one half of a `tokio::io::duplex` and serves the client
/// on the other.
pub fn in_memory<S, F>(make_server: F) -> Arc<dyn TransportFactory>
where
    F: Fn() -> S + Send + Sync + 'static,
    S: ServerHandler,
{
    ScriptedFactory::new(make_server)
}

/// A [`TransportFactory`] over in-memory servers that a test can observe and steer.
///
/// Every connect counts itself and announces itself to [`entered`](ScriptedFactory::entered); while
/// the gate is closed it then waits for [`open_gate`](ScriptedFactory::open_gate) (a dropped
/// connect future stops waiting); then it fails if a failure is set, else serves a fresh server
/// over a duplex.
pub struct ScriptedFactory {
    serve: Option<ServeFn>,
    connects: AtomicUsize,
    /// `true` while the gate is open.
    gate: watch::Sender<bool>,
    /// One permit per connect that has entered and not yet been awaited.
    entered: Semaphore,
    failure: Mutex<Option<String>>,
    /// The most recent connection's server: its token and task.
    last: Mutex<Option<(CancellationToken, JoinHandle<()>)>>,
    open: Arc<AtomicUsize>,
}

/// Counts a running server session; dropped when its task ends.
struct OpenSession(Arc<AtomicUsize>);

impl Drop for OpenSession {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl ScriptedFactory {
    fn build(serve: Option<ServeFn>, failure: Option<String>) -> Arc<Self> {
        Arc::new(Self {
            serve,
            connects: AtomicUsize::new(0),
            gate: watch::Sender::new(true),
            entered: Semaphore::new(0),
            failure: Mutex::new(failure),
            last: Mutex::new(None),
            open: Arc::new(AtomicUsize::new(0)),
        })
    }

    /// Serves a fresh `make_server()` per connect; the gate starts open.
    pub fn new<S, F>(make_server: F) -> Arc<Self>
    where
        F: Fn() -> S + Send + Sync + 'static,
        S: ServerHandler,
    {
        Self::build(Some(serve_fn(make_server)), None)
    }

    /// Every connect fails with a `BoxError` whose text is `msg`.
    pub fn failing(msg: &str) -> Arc<Self> {
        Self::build(None, Some(msg.to_owned()))
    }

    /// How many times `connect` was called.
    pub fn connects(&self) -> usize {
        self.connects.load(Ordering::SeqCst)
    }

    /// Makes later connects wait for [`open_gate`](ScriptedFactory::open_gate).
    pub fn close_gate(&self) {
        self.gate.send_replace(false);
    }

    /// Releases every waiting connect and lets later ones through.
    pub fn open_gate(&self) {
        self.gate.send_replace(true);
    }

    /// Waits until a connect has entered; each entry satisfies one call.
    pub async fn entered(&self) {
        if let Ok(permit) = self.entered.acquire().await {
            permit.forget();
        }
    }

    /// Makes later connects fail with `msg` (`None` serves again).
    pub fn fail_with(&self, msg: Option<&str>) {
        *lock(&self.failure) = msg.map(str::to_owned);
    }

    /// Ends the most recent connection's server side and waits for it, so the client's next request
    /// on it fails with a closed transport.
    pub async fn kill_last(&self) {
        let last = lock(&self.last).take();
        if let Some((ct, task)) = last {
            ct.cancel();
            let _ = task.await;
        }
    }

    /// How many server sessions are still running: a closed client session ends its server session.
    pub fn open_sessions(&self) -> usize {
        self.open.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl TransportFactory for ScriptedFactory {
    async fn connect(
        &self,
        client_info: ClientConfig,
    ) -> Result<RunningService<RoleClient, ClientConfig>, BoxError> {
        self.connects.fetch_add(1, Ordering::SeqCst);
        self.entered.add_permits(1);
        let mut gate = self.gate.subscribe();
        // The sender lives in `self`, so this only returns once the gate is open.
        let _ = gate.wait_for(|open| *open).await;
        if let Some(msg) = lock(&self.failure).clone() {
            return Err(msg.into());
        }
        let Some(serve) = &self.serve else {
            return Err("ScriptedFactory: no server to serve".into());
        };
        let (client_io, server_io) = tokio::io::duplex(DUPLEX_BUFFER);
        let ct = CancellationToken::new();
        self.open.fetch_add(1, Ordering::SeqCst);
        let session = OpenSession(self.open.clone());
        let served = serve(server_io, ct.clone());
        let task = tokio::spawn(async move {
            let _session = session;
            served.await;
        });
        *lock(&self.last) = Some((ct, task));
        serve_client(client_info, client_io).await
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::time::Duration;

    use rmcp::model::CallToolRequestParams;
    use serde_json::json;

    use super::*;
    use crate::config::default_client_info;
    use crate::testutil::weather_server;

    async fn weather_call(
        client: &RunningService<RoleClient, ClientConfig>,
    ) -> Result<(), rmcp::ServiceError> {
        let mut params = CallToolRequestParams::new("get_weather");
        params.arguments = json!({"city": "Paris"}).as_object().cloned();
        client.call_tool(params).await.map(|_| ())
    }

    /// Waits (bounded) until `cond` holds: server tasks end asynchronously.
    async fn eventually(cond: impl Fn() -> bool) {
        for _ in 0..500 {
            if cond() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        panic!("condition not reached");
    }

    #[tokio::test]
    async fn in_memory_serves_a_fresh_server_per_connect() {
        let factory = in_memory(weather_server);
        let first = factory.connect(default_client_info()).await.unwrap();
        let second = factory.connect(default_client_info()).await.unwrap();
        weather_call(&first).await.unwrap();
        weather_call(&second).await.unwrap();
        first.cancel().await.unwrap();
        // The other session is independent of the closed one.
        weather_call(&second).await.unwrap();
        second.cancel().await.unwrap();
    }

    #[tokio::test]
    async fn scripted_counts_connects_and_tracks_open_sessions() {
        let factory = ScriptedFactory::new(weather_server);
        assert_eq!((factory.connects(), factory.open_sessions()), (0, 0));
        let first = factory.connect(default_client_info()).await.unwrap();
        let second = factory.connect(default_client_info()).await.unwrap();
        assert_eq!((factory.connects(), factory.open_sessions()), (2, 2));
        // Each connect entered once.
        factory.entered().await;
        factory.entered().await;

        // Closing a client session ends its server session (Go server.Sessions()).
        first.cancel().await.unwrap();
        eventually(|| factory.open_sessions() == 1).await;
        second.cancel().await.unwrap();
        eventually(|| factory.open_sessions() == 0).await;
    }

    #[tokio::test]
    async fn scripted_gate_holds_connects_until_opened() {
        let factory = ScriptedFactory::new(weather_server);
        factory.close_gate();
        let pending = tokio::spawn({
            let factory = factory.clone();
            async move { factory.connect(default_client_info()).await }
        });
        factory.entered().await;
        assert_eq!(factory.connects(), 1);
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!pending.is_finished(), "connect passed a closed gate");
        assert_eq!(factory.open_sessions(), 0);

        factory.open_gate();
        let client = pending.await.unwrap().unwrap();
        weather_call(&client).await.unwrap();
        client.cancel().await.unwrap();
    }

    #[tokio::test]
    async fn scripted_dropped_connect_stops_waiting_at_the_gate() {
        let factory = ScriptedFactory::new(weather_server);
        factory.close_gate();
        let connect = factory.connect(default_client_info());
        let timed_out = tokio::time::timeout(Duration::from_millis(20), connect).await;
        assert!(timed_out.is_err(), "connect passed a closed gate");
        factory.entered().await;
        assert_eq!((factory.connects(), factory.open_sessions()), (1, 0));

        // A later connect waits for the gate too, then gets through.
        let factory2 = factory.clone();
        let pending = tokio::spawn(async move { factory2.connect(default_client_info()).await });
        factory.entered().await;
        factory.open_gate();
        let client = pending.await.unwrap().unwrap();
        assert_eq!((factory.connects(), factory.open_sessions()), (2, 1));
        client.cancel().await.unwrap();
    }

    #[tokio::test]
    async fn scripted_failures_return_the_message() {
        let factory = ScriptedFactory::failing("dial failed");
        let err = factory.connect(default_client_info()).await.unwrap_err();
        assert_eq!(err.to_string(), "dial failed");
        assert_eq!((factory.connects(), factory.open_sessions()), (1, 0));

        let factory = ScriptedFactory::new(weather_server);
        factory.fail_with(Some("server down"));
        let err = factory.connect(default_client_info()).await.unwrap_err();
        assert_eq!(err.to_string(), "server down");
        factory.fail_with(None);
        let client = factory.connect(default_client_info()).await.unwrap();
        weather_call(&client).await.unwrap();
        assert_eq!(factory.connects(), 2);
        client.cancel().await.unwrap();
    }

    #[tokio::test]
    async fn scripted_kill_last_closes_the_last_connection() {
        let factory = ScriptedFactory::new(weather_server);
        let first = factory.connect(default_client_info()).await.unwrap();
        let last = factory.connect(default_client_info()).await.unwrap();
        factory.kill_last().await;
        assert_eq!(factory.open_sessions(), 1);
        let err = weather_call(&last).await.unwrap_err();
        assert!(
            matches!(err, rmcp::ServiceError::TransportClosed),
            "want TransportClosed, got {err:?}"
        );
        // The earlier connection is untouched, and killing again is a no-op.
        weather_call(&first).await.unwrap();
        factory.kill_last().await;
        weather_call(&first).await.unwrap();
        first.cancel().await.unwrap();
    }
}
