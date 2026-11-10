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

//! Ports of ADK-Go `server/adkrest/sse_timeout_integration_test.go`: `/run_sse` on a whole
//! [`Server`] on a real loopback listener streams the model's answer under every
//! [`ServerConfig::sse_write_timeout`] setting.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use gadk_agent::SingleLoader;
use gadk_engine::llm::LlmAgentConfig;
use gadk_genai::Content;
use gadk_server::rest::testutil::wire_scenario::ScriptedModel;
use gadk_server::rest::testutil::{authenticate_as, serve_loopback};
use gadk_server::rest::{Server, ServerConfig, Timeout};
use gadk_session::InMemorySessionService;
use serde_json::{Value, json};

const COMPACTION_APP: &str = "compaction_app";
const COMPACTION_USER: &str = "u";

/// Go `newSSETimeoutServer` served on 127.0.0.1: an LLM agent whose model answers `answer 1`,
/// every caller authenticated as `u`, and `timeout` as the SSE write timeout.
async fn new_sse_timeout_server(timeout: Timeout) -> SocketAddr {
    let root = gadk_engine::llm_agent::new(LlmAgentConfig {
        name: COMPACTION_APP.into(),
        model: Some(ScriptedModel::new("echo", |_| {
            Ok(Content::from_text("answer 1", "model"))
        })),
        ..Default::default()
    })
    .unwrap();
    let mut cfg = ServerConfig::new(Arc::new(InMemorySessionService::new()));
    cfg.agent_loader = Some(Arc::new(SingleLoader::new(root)));
    cfg.sse_write_timeout = timeout;
    cfg.authenticator = Some(authenticate_as(COMPACTION_USER));
    serve_loopback(Server::new(cfg).unwrap().router())
        .await
        .unwrap()
}

/// Go `createCompactionSession`: creates a session over HTTP and returns its ID.
async fn create_session(client: &reqwest::Client, addr: SocketAddr) -> String {
    let resp = client
        .post(format!(
            "http://{addr}/apps/{COMPACTION_APP}/users/{COMPACTION_USER}/sessions"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200, "POST sessions status");
    let session: Value = resp.json().await.unwrap();
    session["id"].as_str().unwrap().to_owned()
}

/// Go `postRunSSE`: the whole `/run_sse` body of a `hi` turn; fails unless the status is 200.
async fn post_run_sse(client: &reqwest::Client, addr: SocketAddr, session_id: &str) -> String {
    let resp = client
        .post(format!("http://{addr}/run_sse"))
        .json(&json!({
            "appName": COMPACTION_APP,
            "userId": COMPACTION_USER,
            "sessionId": session_id,
            "newMessage": {"role": "user", "parts": [{"text": "hi"}]},
        }))
        .send()
        .await
        .unwrap();
    let status = resp.status().as_u16();
    let body = resp.text().await.unwrap();
    assert_eq!(status, 200, "POST /run_sse status; body: {body}");
    body
}

// parity: adapted: gadk's SSE write deadline is a timer inside the response stream, not a SetWriteDeadline call a wrapping ResponseWriter can record, so each case checks that the stream delivers the model's answer under the setting (a zero timeout no longer failing before the first byte); the deadline ending a stream is covered by server_adkrest_controllers_runtime.rs's sse_write_timeout_ends_the_stream
#[tokio::test]
async fn test_run_sse_write_timeout() {
    for (name, timeout) in [
        ("zero uses default", Timeout::Default),
        ("positive", Timeout::After(Duration::from_secs(30))),
        ("negative clears deadline", Timeout::Disabled),
    ] {
        let addr = new_sse_timeout_server(timeout).await;
        let client = reqwest::Client::new();
        let sid = create_session(&client, addr).await;
        let got = post_run_sse(&client, addr, &sid).await;
        assert!(
            got.contains("answer 1"),
            "{name}: POST /run_sse body does not contain the model's answer:\n{got}"
        );
    }
}
