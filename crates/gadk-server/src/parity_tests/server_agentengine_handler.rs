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

//! Ports of ADK-Go `server/agentengine/handler_test.go`.

use std::sync::{Arc, Mutex};

use axum::body::{Body, to_bytes};
use gadk_platform::GoDuration;
use gadk_session::InMemorySessionService;
use gadk_session::compaction::Config as CompactionConfig;
use http::Method;
use http::header::CONTENT_TYPE;

use crate::agentengine::{AgentEngineConfig, new_handler};
use crate::mux::{Request, WriteDeadline};
use crate::rest::testutil::single_agent_loader;

fn config(compaction: Option<CompactionConfig>) -> AgentEngineConfig {
    let mut cfg = AgentEngineConfig::new(
        Arc::new(InMemorySessionService::new()),
        single_agent_loader("engine"),
    );
    cfg.compaction = compaction;
    cfg
}

// parity: adapted: AgentEngineConfig requires an agent loader (Go's config has none), so a bare root agent is loaded; the shape check rejects both bad configs before the root agent is consulted
#[test]
fn test_new_handler_rejects_unusable_compaction() {
    let cases: Vec<(&str, Option<CompactionConfig>, bool)> = vec![
        ("nil compaction is fine", None, true),
        (
            "overlap with no interval",
            Some(CompactionConfig {
                overlap_size: 2,
                ..Default::default()
            }),
            false,
        ),
        (
            "no strategy at all",
            Some(CompactionConfig::default()),
            false,
        ),
    ];
    for (name, compaction, ok) in cases {
        let result = new_handler(config(compaction), GoDuration::SECOND, 1 << 20, "engine");
        match (result, ok) {
            (Ok(_), true) => {}
            (Err(e), true) => panic!("{name}: NewHandler() = {e}, want nil"),
            (Ok(_), false) => {
                panic!("{name}: NewHandler() accepted a compaction config it cannot serve")
            }
            (Err(e), false) => assert!(
                e.to_string().contains("Compaction"),
                "{name}: error {:?} does not name the field an operator has to change",
                e.to_string()
            ),
        }
    }
}

// parity: adapted: the handler is called directly with a recording WriteDeadline request extension (Go: an httptest server and a SetWriteDeadline-recording ResponseWriter)
#[tokio::test]
async fn test_query_sse_write_timeout() {
    let cases = [
        (
            "zero uses default",
            GoDuration(0),
            Some(std::time::Duration::from_secs(120)),
        ),
        (
            "positive",
            GoDuration::from_secs(30),
            Some(std::time::Duration::from_secs(30)),
        ),
        ("negative clears deadline", GoDuration(-1), None),
    ];
    for (name, timeout, want_deadline) in cases {
        let h = new_handler(config(None), timeout, 1 << 20, "engine")
            .unwrap_or_else(|e| panic!("{name}: NewHandler() error = {e}"));
        let recorded: Arc<Mutex<Vec<Option<tokio::time::Instant>>>> = Arc::default();
        let sink = Arc::clone(&recorded);
        let mut req = Request::new(Body::from(
            r#"{"class_method":"async_create_session","input":{"user_id":"u"}}"#,
        ));
        *req.method_mut() = Method::POST;
        *req.uri_mut() = "/reasoning_engine".parse().unwrap();
        req.headers_mut()
            .insert(CONTENT_TYPE, "application/json".parse().unwrap());
        req.extensions_mut()
            .insert(WriteDeadline::new(move |d| sink.lock().unwrap().push(d)));

        let start = tokio::time::Instant::now();
        let resp = h(req).await;
        let end = tokio::time::Instant::now();
        let status = resp.status().as_u16();
        let got = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let got = String::from_utf8(got.to_vec()).unwrap();
        assert_eq!(
            status, 200,
            "POST /reasoning_engine status = {status}, want 200; body: {got}"
        );
        assert!(
            got.contains(r#""user_id":"u""#),
            "response does not contain the created session:\n{got}"
        );

        let deadlines = recorded.lock().unwrap().clone();
        assert_eq!(
            deadlines.len(),
            1,
            "{name}: SetWriteDeadline called {} times, want 1",
            deadlines.len()
        );
        match want_deadline {
            None => assert_eq!(deadlines[0], None, "{name}: want no deadline"),
            Some(want) => {
                let d = deadlines[0].unwrap_or_else(|| panic!("{name}: no deadline"));
                assert!(
                    d >= start + want && d <= end + want,
                    "{name}: write deadline = {d:?}, want between {:?} and {:?}",
                    start + want,
                    end + want
                );
            }
        }
    }
}
