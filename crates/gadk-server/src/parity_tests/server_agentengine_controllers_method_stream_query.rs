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

//! Ports of ADK-Go `server/agentengine/controllers/method/stream_query_test.go`.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::body::to_bytes;
use bytes::Bytes;
use gadk_agent::{AgentError, BeforeAgentCallback, SingleLoader};
use gadk_engine::llm::LlmAgentConfig;
use gadk_genai::Content;
use gadk_session::compaction::{Config as CompactionConfig, SummarizeResult, Summarizer};
use gadk_session::{CreateRequest, Event, InMemorySessionService};
use serde::Deserialize;

use crate::agentengine::AgentEngineConfig;
use crate::agentengine::method::stream_query::StreamQueryHandler;
use crate::agentengine::method::streaming_agent_run_with_events::StreamingAgentRunWithEventsHandler;
use crate::mux::Response;

/// Go `simpleEvent`: only the event's content.
#[derive(Deserialize)]
struct SimpleEvent {
    content: Option<Content>,
}

/// Go's `Echo` agent: its before-agent callback answers with the user's message.
fn echo_config() -> AgentEngineConfig {
    let echo: BeforeAgentCallback = Arc::new(|ctx| {
        let content = ctx.user_content().cloned();
        Box::pin(async move { Ok::<_, AgentError>(content) })
    });
    let agent = gadk_engine::llm_agent::new(LlmAgentConfig {
        name: "Echo".into(),
        before_agent_callbacks: vec![echo],
        ..Default::default()
    })
    .unwrap_or_else(|e| panic!("failed to create agent: {e}"));
    AgentEngineConfig::new(
        Arc::new(InMemorySessionService::new()),
        Arc::new(SingleLoader::new(agent)),
    )
}

/// The NDJSON body Go's `stringWriter` collects.
async fn body_text(resp: Response) -> String {
    let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    String::from_utf8(body.to_vec()).unwrap()
}

/// Go: a simple message as string gives the same result as `genai.Content`.
#[tokio::test]
async fn test_simple_text() {
    let app_name = 123.to_string();
    let user_id = "u";
    let config = echo_config();
    let h = StreamQueryHandler::new(config.clone(), &app_name, "async_stream_query", "");

    let sess = config
        .session_service
        .create(CreateRequest {
            app_name: app_name.clone(),
            user_id: user_id.into(),
            ..Default::default()
        })
        .await
        .unwrap_or_else(|e| panic!("failed to create session: {e}"));
    let want = Content::from_text("Say hello", "user");

    let tests = [
        (
            "full content",
            format!(
                "{{\n\"class_method\":\"async_stream_query\",\n\"input\":{{\n   \"message\":{{\n     \"parts\":[\n        {{\"text\":\"Say hello\"}}\n      ],\n      \"role\":\"user\"\n   }},\n   \"session_id\":\"{}\",\n   \"user_id\":\"{user_id}\"}}}}",
                sess.id()
            ),
        ),
        (
            "simplified content",
            format!(
                "{{\n\"class_method\":\"async_stream_query\",\n\"input\":{{\n   \"message\":\"Say hello\",\n   \"session_id\":\"{}\",\n   \"user_id\":\"{user_id}\"}}}}",
                sess.id()
            ),
        ),
    ];

    for (name, payload) in tests {
        let resp = h
            .stream_jsonl(Bytes::from(payload))
            .await
            .unwrap_or_else(|e| panic!("{name}: streamJSONL() failed: {e}"));
        let p = body_text(resp).await;
        let ev: SimpleEvent = serde_json::from_str(&p)
            .unwrap_or_else(|e| panic!("{name}: json.Unmarshal() failed: {e}\n{p}"));
        assert_eq!(
            ev.content.as_ref(),
            Some(&want),
            "{name}: streamJSONL() = {p}"
        );
    }
}

/// Go `countingSummarizer`: records that compaction reached the summarizer.
#[derive(Default)]
struct CountingSummarizer {
    calls: AtomicUsize,
}

#[async_trait::async_trait]
impl Summarizer for CountingSummarizer {
    async fn summarize_events(
        &self,
        _events: &[Event],
    ) -> Result<SummarizeResult, Box<dyn std::error::Error + Send + Sync>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(SummarizeResult {
            content: Some(Content::from_text("a summary", "model")),
            usage: None,
        })
    }
}

/// Go: the compaction config this surface accepts reaches the runners it builds (both methods
/// build a `runner.Config` per request).
#[tokio::test]
async fn test_agent_engine_actually_runs_compaction() {
    const APP_NAME: &str = "123";
    const USER_ID: &str = "u";
    for name in ["stream_query", "streaming_agent_run_with_events"] {
        let summarizer = Arc::new(CountingSummarizer::default());
        let mut config = echo_config();
        config.compaction = Some(CompactionConfig {
            compaction_interval: 1,
            summarizer: Some(summarizer.clone()),
            ..Default::default()
        });

        let sess = config
            .session_service
            .create(CreateRequest {
                app_name: APP_NAME.into(),
                user_id: USER_ID.into(),
                ..Default::default()
            })
            .await
            .unwrap_or_else(|e| panic!("{name}: Create() error = {e}"));
        let session_id = sess.id().to_owned();

        let resp = if name == "stream_query" {
            let payload = format!(
                r#"{{"class_method":"async_stream_query","input":{{"message":{{"parts":[{{"text":"hi"}}],"role":"user","role":"user"}},"session_id":"{session_id}","user_id":"{USER_ID}"}}}}"#
            );
            StreamQueryHandler::new(config, APP_NAME, "async_stream_query", "")
                .stream_jsonl(Bytes::from(payload))
                .await
        } else {
            // This method carries its request as a JSON string inside the envelope rather than
            // as an object.
            let inner = serde_json::json!({
                "message": {"role": "user", "parts": [{"text": "hi"}]},
                "session_id": session_id,
                "user_id": USER_ID,
            })
            .to_string();
            let outer = serde_json::json!({
                "class_method": "async_stream_agent_run_with_events",
                "input": {"request_json": inner},
            })
            .to_string();
            StreamingAgentRunWithEventsHandler::new(
                config,
                APP_NAME,
                "async_stream_agent_run_with_events",
                "",
            )
            .stream_jsonl(Bytes::from(outer))
            .await
        };
        let resp = resp.unwrap_or_else(|e| panic!("{name}: streamJSONL() error = {e}"));
        // Go's loop runs while it writes; the Rust body runs as it is read.
        body_text(resp).await;
        assert!(
            summarizer.calls.load(Ordering::SeqCst) > 0,
            "{name}: the summarizer was never called, so this surface accepts a compaction config and does nothing with it"
        );
    }
}
