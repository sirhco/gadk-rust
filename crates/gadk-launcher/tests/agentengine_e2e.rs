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

//! End to end through a real server: `agentengine::new_launcher` (Go `cmd/launcher/agentengine`)
//! answering a stream query (spec R11), and the web server cutting a stream that outlives the
//! handler's `-sse-write-timeout` write deadline (spec R4).

#![cfg(feature = "agentengine")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt as _;
use futures::stream::BoxStream;
use gadk_agent::SingleLoader;
use gadk_genai::{Content, FinishReason, ROLE_MODEL};
use gadk_launcher::agentengine::new_launcher;
use gadk_launcher::{CancellationToken, Config, Launcher, golog};
use gadk_model::{Llm, LlmRequest, LlmResponse, ModelError};
use pretty_assertions::assert_eq;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::task::JoinHandle;

/// Runs `new_launcher("test-engine")` with `web -port <port> agentengine <flags>` over `agent`
/// until the returned token is cancelled.
async fn start(
    agent: Arc<dyn gadk_agent::Agent>,
    flags: &[&str],
) -> (u16, CancellationToken, JoinHandle<()>) {
    let port = support::free_port();
    let mut args = support::strings(&["web", "-port", &port.to_string(), "agentengine"]);
    args.extend(support::strings(flags));
    let mut config = Config::new(Arc::new(SingleLoader::new(agent)));
    config.log = golog::capture().0;
    let cancel = CancellationToken::new();
    let running = {
        let cancel = cancel.clone();
        tokio::spawn(async move {
            let mut l = new_launcher("test-engine");
            l.execute(cancel, config, args).await.unwrap();
        })
    };
    support::wait_listening(port).await;
    (port, cancel, running)
}

/// `POST <target>` with a JSON `body` and `Connection: close`; the raw response, read until the
/// server closes the connection (cleanly or not).
async fn post(port: u16, target: &str, body: &str) -> Vec<u8> {
    let mut conn = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let request = format!(
        "POST {target} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    conn.write_all(request.as_bytes()).await.unwrap();
    let mut raw = Vec::new();
    let mut buf = [0u8; 4096];
    let read_all = async {
        loop {
            match conn.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => raw.extend_from_slice(&buf[..n]),
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(10), read_all)
        .await
        .expect("the server did not end the response");
    raw
}

/// R11: Go's `agentengine.NewLauncher(id)` serves the scenario agent's stream query under
/// `/api` (one NDJSON line per event, Go's headers).
#[tokio::test]
async fn new_launcher_serves_a_stream_query() {
    let (port, cancel, running) = start(
        gadk_server::rest::testutil::wire_scenario::assistant_agent(),
        &[],
    )
    .await;
    let raw = post(
        port,
        "/api/stream_reasoning_engine",
        r#"{"class_method":"async_stream_query","input":{"user_id":"u1","message":"hello"}}"#,
    )
    .await;
    let resp = support::parse_response(&raw, false, port);
    assert_eq!(resp["status"], 200, "{resp}");
    let headers: Vec<(String, String)> = serde_json::from_value(resp["headers"].clone()).unwrap();
    for want in [
        ("Cache-Control", "no-cache"),
        ("Connection", "close"),
        ("Content-Type", "application/json"),
        ("Transfer-Encoding", "chunked"),
    ] {
        assert!(
            headers.contains(&(want.0.to_owned(), want.1.to_owned())),
            "{want:?} in {headers:?}"
        );
    }
    let body = resp["body"].as_str().unwrap();
    let lines: Vec<serde_json::Value> = body
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 1, "{body}");
    assert_eq!(lines[0]["author"], "assistant");
    assert_eq!(
        lines[0]["content"]["parts"][0]["text"],
        "Hello! How can I help?"
    );
    assert_eq!(lines[0]["finish_reason"], "STOP");

    cancel.cancel();
    running.await.unwrap();
}

/// A model that streams one chunk at once and its final answer only after `delay`.
struct SlowModel {
    delay: Duration,
}

impl Llm for SlowModel {
    fn name(&self) -> &str {
        "slow"
    }

    fn generate_content(
        &self,
        _req: LlmRequest,
        _stream: bool,
    ) -> BoxStream<'static, Result<LlmResponse, ModelError>> {
        let delay = self.delay;
        let first = futures::stream::once(async {
            Ok(LlmResponse {
                content: Some(Content::from_text("first ", ROLE_MODEL)),
                partial: true,
                ..Default::default()
            })
        });
        let last = futures::stream::once(async move {
            tokio::time::sleep(delay).await;
            Ok(LlmResponse {
                content: Some(Content::from_text("first second", ROLE_MODEL)),
                finish_reason: Some(FinishReason::Stop),
                ..Default::default()
            })
        });
        first.chain(last).boxed()
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// R4: with `-sse-write-timeout 500ms` the handler's write deadline (now + 500 ms) is the real
/// server's: the first event's line is delivered, the line written after the deadline (the
/// model's next response, 3 s later) is not, and the connection is aborted without the chunked
/// body's terminator. The margins are wide so a slow debug first event on a loaded machine still
/// lands before the deadline.
#[tokio::test]
async fn sse_write_timeout_cuts_a_slow_stream() {
    let agent = support::probe_agent_with(Arc::new(SlowModel {
        delay: Duration::from_secs(3),
    }));
    let (port, cancel, running) = start(agent, &["-sse-write-timeout", "500ms"]).await;
    let started = std::time::Instant::now();
    let raw = post(
        port,
        "/api/stream_reasoning_engine",
        r#"{"class_method":"async_stream_query","input":{"user_id":"u1","message":"hi"}}"#,
    )
    .await;
    let elapsed = started.elapsed();
    let text = String::from_utf8_lossy(&raw).into_owned();
    assert!(text.starts_with("HTTP/1.1 200 OK\r\n"), "{text}");
    assert!(text.contains(r#""text":"first ""#), "{text}");
    assert!(!text.contains("first second"), "{text}");
    assert!(!text.ends_with("0\r\n\r\n"), "{text}");
    assert!(elapsed < Duration::from_secs(10), "{elapsed:?}");

    // Without the timeout cut the same stream completes.
    let agent = support::probe_agent_with(Arc::new(SlowModel {
        delay: Duration::from_millis(200),
    }));
    let (port2, cancel2, running2) = start(agent, &[]).await;
    let raw = post(
        port2,
        "/api/stream_reasoning_engine",
        r#"{"class_method":"async_stream_query","input":{"user_id":"u1","message":"hi"}}"#,
    )
    .await;
    let text = String::from_utf8_lossy(&raw).into_owned();
    assert!(text.contains("first second"), "{text}");
    assert!(text.ends_with("0\r\n\r\n"), "{text}");

    cancel.cancel();
    cancel2.cancel();
    running.await.unwrap();
    running2.await.unwrap();
}
