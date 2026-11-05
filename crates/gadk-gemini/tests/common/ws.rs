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

//! An in-process WebSocket server for the Live tests: accepts one connection on
//! `127.0.0.1:0`, records the handshake path and headers and every frame the client sends,
//! and answers the client's first frame (the setup) with scripted frames.

use std::sync::Arc;
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use http::HeaderMap;
use parking_lot::Mutex;
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{
    Callback, ErrorResponse, Request, Response,
};

/// What the server has observed.
#[derive(Default)]
pub struct Seen {
    pub path: Option<String>,
    pub headers: HeaderMap,
    pub frames: Vec<Value>,
    pub closed: bool,
}

pub struct WsServer {
    pub port: u16,
    pub seen: Arc<Mutex<Seen>>,
}

/// The handshake callback: records the request path and headers.
struct Capture(Arc<Mutex<Seen>>);

impl Callback for Capture {
    #[allow(clippy::result_large_err)]
    fn on_request(self, req: &Request, resp: Response) -> Result<Response, ErrorResponse> {
        let mut s = self.0.lock();
        s.path = Some(req.uri().path().to_string());
        s.headers = req.headers().clone();
        Ok(resp)
    }
}

impl WsServer {
    /// Starts a server whose first scripted frame is `{"setupComplete":{}}`, followed by
    /// `frames`.
    pub async fn start(frames: Vec<Value>) -> Self {
        let mut script = vec![json!({"setupComplete": {}})];
        script.extend(frames);
        Self::start_raw(script).await
    }

    /// Starts a server that answers the setup with exactly `script`.
    pub async fn start_raw(script: Vec<Value>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = Arc::new(Mutex::new(Seen::default()));
        let task_seen = seen.clone();
        tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let ws = tokio_tungstenite::accept_hdr_async(tcp, Capture(task_seen.clone()))
                .await
                .unwrap();
            let (mut sink, mut stream) = ws.split();
            let mut script = Some(script);
            while let Some(msg) = stream.next().await {
                let Ok(msg) = msg else { break };
                match msg {
                    Message::Text(t) => {
                        task_seen
                            .lock()
                            .frames
                            .push(serde_json::from_str(t.as_str()).unwrap());
                    }
                    Message::Binary(b) => {
                        task_seen
                            .lock()
                            .frames
                            .push(serde_json::from_slice(&b).unwrap());
                    }
                    Message::Close(_) => {
                        // Keep polling: tungstenite sends the close reply, then ends.
                        task_seen.lock().closed = true;
                        continue;
                    }
                    _ => continue,
                }
                if let Some(script) = script.take() {
                    for frame in script {
                        if sink.send(Message::text(frame.to_string())).await.is_err() {
                            break;
                        }
                    }
                }
            }
        });
        Self { port, seen }
    }

    pub fn base_url(&self) -> String {
        format!("ws://127.0.0.1:{}/", self.port)
    }

    /// Waits (up to 5s) until the server has received `n` frames, and returns them.
    pub async fn frames(&self, n: usize) -> Vec<Value> {
        for _ in 0..500 {
            {
                let s = self.seen.lock();
                if s.frames.len() >= n {
                    return s.frames.clone();
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!(
            "expected {n} frames, got {:?}",
            self.seen.lock().frames.clone()
        );
    }

    /// Waits (up to 5s) until the client has sent a close frame.
    pub async fn wait_closed(&self) -> bool {
        for _ in 0..500 {
            if self.seen.lock().closed {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        false
    }

    pub fn path(&self) -> Option<String> {
        self.seen.lock().path.clone()
    }

    pub fn headers(&self) -> HeaderMap {
        self.seen.lock().headers.clone()
    }
}
