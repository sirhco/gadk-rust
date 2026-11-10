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

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Ports of ADK-Go `server/adkrest/runlive_limits_integration_test.go`: each `/run_live` limit
//! set on [`ServerConfig`] reaches the handler that enforces it, checked on a whole [`Server`]
//! on a real loopback listener with a live agent that holds the connection open.

use std::any::Any;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::{SinkExt, StreamExt};
use gadk_agent::{
    AgentConfig, AgentError, AgentState, AgentType, EventStream, InvocationContext, LiveAgent,
    LiveRequest, LiveSession, SingleLoader,
};
use gadk_artifact::InMemoryArtifactService;
use gadk_memory::InMemoryMemoryService;
use gadk_server::rest::testutil::serve_loopback;
use gadk_server::rest::{Server, ServerConfig, Timeout};
use gadk_session::{CreateRequest, InMemorySessionService, SessionService};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Error as WsError;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_util::sync::CancellationToken;

const GUARD_TEST_APP: &str = "test-agent";

/// Go `closableLiveSession`: accepts every request; `close` ends the run's stream.
struct ClosableLiveSession(CancellationToken);

#[async_trait]
impl LiveSession for ClosableLiveSession {
    async fn send(&self, _req: LiveRequest) -> Result<(), AgentError> {
        Ok(())
    }
    async fn close(&self) -> Result<(), AgentError> {
        self.0.cancel();
        Ok(())
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Go `heldLiveAgent`: a live run that yields nothing and ends when its session is closed.
struct HeldLiveAgent;

#[async_trait]
impl LiveAgent for HeldLiveAgent {
    async fn run_live(
        &self,
        _ctx: InvocationContext,
    ) -> Result<(Arc<dyn LiveSession>, EventStream), AgentError> {
        let closed = CancellationToken::new();
        let session: Arc<dyn LiveSession> = Arc::new(ClosableLiveSession(closed.clone()));
        let events = futures::stream::once(async move { closed.cancelled().await })
            .filter_map(|()| async { None })
            .boxed();
        Ok((session, events))
    }
}

/// Go `serveHeldRunLive`: serves `cfg` around a held live agent on 127.0.0.1 with session
/// `u1`/`s1` existing, and returns the listener's address and the `/run_live` URL.
async fn serve_held_run_live(mut cfg: ServerConfig) -> (SocketAddr, String) {
    let root = gadk_agent::new_with_state(
        AgentConfig {
            name: GUARD_TEST_APP.into(),
            description: "root agent".into(),
            ..Default::default()
        },
        AgentState {
            agent_type: AgentType::CustomAgent,
            config: None,
            on_before_agent_callback_event: None,
            live: Some(Arc::new(HeldLiveAgent)),
        },
    )
    .unwrap();
    // The session has to exist, or the run fails at once and the session-limit case would
    // pass for the wrong reason.
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: GUARD_TEST_APP.into(),
            user_id: "u1".into(),
            session_id: Some("s1".into()),
            state: None,
        })
        .await
        .unwrap();
    cfg.session_service = sessions;
    cfg.memory_service = Some(Arc::new(InMemoryMemoryService::new()));
    cfg.artifact_service = Some(Arc::new(InMemoryArtifactService::new()));
    cfg.agent_loader = Some(Arc::new(SingleLoader::new(root)));
    cfg.bind_host = "127.0.0.1".into();
    let addr = serve_loopback(Server::new(cfg).unwrap().router())
        .await
        .unwrap();
    let url = format!("ws://{addr}/run_live?appName={GUARD_TEST_APP}&userId=u1&sessionId=s1");
    (addr, url)
}

/// A config with only the limits set (the rest is filled by [`serve_held_run_live`]).
fn limits_config() -> ServerConfig {
    ServerConfig::new(Arc::new(InMemorySessionService::new()))
}

type Client =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Go `dialHeldRunLive`: a `/run_live` connection whose handshake was accepted.
async fn dial_held_run_live(ws_url: &str) -> Client {
    let (conn, resp) = tokio_tungstenite::connect_async(ws_url).await.unwrap();
    assert_eq!(resp.status().as_u16(), 101, "handshake status");
    conn
}

// parity: adapted: the subtests run in sequence in one test, and the LiveKeepaliveTimeout case's client is a raw socket that answers no ping (tungstenite would pong while reading; gorilla's test overrides its ping handler)
#[tokio::test]
async fn test_run_live_limits_from_server_config() {
    // MaxLiveMessageBytes: an oversized message closes the connection with 1009.
    {
        let mut cfg = limits_config();
        cfg.max_live_message_bytes = 64;
        let (_, url) = serve_held_run_live(cfg).await;
        let mut conn = dial_held_run_live(&url).await;
        conn.send(Message::Binary(vec![0u8; 128].into()))
            .await
            .unwrap();
        let msg = timeout(Duration::from_secs(5), conn.next())
            .await
            .expect("MaxLiveMessageBytes: no answer to an oversized message");
        match msg {
            Some(Ok(Message::Close(Some(frame)))) => assert_eq!(
                frame.code,
                CloseCode::Size,
                "MaxLiveMessageBytes: close code"
            ),
            other => panic!(
                "MaxLiveMessageBytes: read after an oversized message = {other:?}, want close code 1009"
            ),
        }
    }

    // LiveKeepaliveTimeout: a client that keeps reading but answers no ping is dropped.
    // The client is a raw socket that never answers a ping; any end of the connection within
    // 5 s counts.
    {
        let mut cfg = limits_config();
        cfg.live_keepalive_timeout = Timeout::After(Duration::from_millis(100));
        let (addr, _) = serve_held_run_live(cfg).await;
        let mut s = TcpStream::connect(addr).await.unwrap();
        let req = format!(
            "GET /run_live?appName={GUARD_TEST_APP}&userId=u1&sessionId=s1 HTTP/1.1\r\nHost: {addr}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n"
        );
        s.write_all(req.as_bytes()).await.unwrap();
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            head.push(s.read_u8().await.unwrap());
        }
        assert!(
            head.starts_with(b"HTTP/1.1 101"),
            "LiveKeepaliveTimeout: handshake: {}",
            String::from_utf8_lossy(&head)
        );
        let dropped = timeout(Duration::from_secs(5), async {
            let mut buf = [0u8; 1024];
            loop {
                match s.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
            }
        })
        .await;
        assert!(
            dropped.is_ok(),
            "LiveKeepaliveTimeout: a client that answers no pings was not dropped"
        );
    }

    // MaxLiveSessions: past the cap the handshake is refused with 503.
    {
        let mut cfg = limits_config();
        cfg.max_live_sessions = 1;
        let (_, url) = serve_held_run_live(cfg).await;
        // The slot is taken before the upgrade, so a completed handshake holds it.
        let _first = dial_held_run_live(&url).await;
        match tokio_tungstenite::connect_async(url.as_str()).await {
            Err(WsError::Http(resp)) => assert_eq!(
                resp.status().as_u16(),
                503,
                "MaxLiveSessions: second handshake status"
            ),
            Err(e) => panic!("MaxLiveSessions: second Dial() error = {e}, want a bad handshake"),
            Ok(_) => panic!("MaxLiveSessions: second Dial() succeeded, want a bad handshake"),
        }
    }
}
