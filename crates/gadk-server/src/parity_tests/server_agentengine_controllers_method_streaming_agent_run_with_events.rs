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

//! Ports of ADK-Go `server/agentengine/controllers/method/streaming_agent_run_with_events_test.go`.

use std::sync::Arc;

use axum::body::to_bytes;
use bytes::Bytes;
use futures::stream::{self, BoxStream};
use gadk_agent::{AgentError, BeforeAgentCallback, Loader, SingleLoader};
use gadk_engine::llm::LlmAgentConfig;
use gadk_genai::{Content, Part};
use gadk_model::{Llm, LlmRequest, LlmResponse, ModelError};
use gadk_session::compaction::{Config as CompactionConfig, SummarizeResult, Summarizer};
use gadk_session::{CreateRequest, Event, GetRequest, InMemorySessionService, ListRequest};
use pretty_assertions::assert_eq;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::agentengine::AgentEngineConfig;
use crate::agentengine::encode_shapes;
use crate::agentengine::method::MethodHandler;
use crate::agentengine::method::streaming_agent_run_with_events::{
    StreamingAgentRunWithEventsHandler, decode_streaming_agent_run_with_events_request,
};
use crate::agentengine::models::{
    StreamingAgentRunWithEventsRequest, StreamingAgentRunWithEventsRunRequest,
};
use crate::agentengine::unmarshal::go_unmarshal;
use crate::rest::testutil::single_agent_loader;

const APP_NAME: &str = "app";
const USER_ID: &str = "test-user@example.com";
const EXTERNAL_SESSION_ID: &str = "projects/111111111111/locations/global/collections/default_collection/engines/test-engine/sessions/12345678901234567890";

/// Go `simpleEvent`.
#[derive(Deserialize)]
struct SimpleEvent {
    content: Option<Content>,
}

/// Go `agentSpaceStreamResponse`.
#[derive(Deserialize)]
struct AgentSpaceStreamResponse {
    #[serde(default)]
    events: Vec<SimpleEvent>,
    #[serde(default)]
    session_id: String,
}

/// Go `streamAwareLLM`: a partial response first when streaming, then the final one.
struct StreamAwareLlm;

impl Llm for StreamAwareLlm {
    fn name(&self) -> &str {
        "stream-aware-llm"
    }

    fn generate_content(
        &self,
        _req: LlmRequest,
        stream: bool,
    ) -> BoxStream<'static, Result<LlmResponse, ModelError>> {
        let mut items = Vec::new();
        if stream {
            items.push(Ok(LlmResponse {
                content: Some(Content::from_text("partial response", "model")),
                partial: true,
                ..Default::default()
            }));
        }
        items.push(Ok(LlmResponse {
            content: Some(Content::from_text("final response", "model")),
            ..Default::default()
        }));
        Box::pin(stream::iter(items))
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// Go's `Echo` agent: its before-agent callback answers with the user's message.
fn echo_loader() -> Arc<dyn Loader> {
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
    Arc::new(SingleLoader::new(agent))
}

fn config(loader: Arc<dyn Loader>) -> AgentEngineConfig {
    AgentEngineConfig::new(Arc::new(InMemorySessionService::new()), loader)
}

/// Go `json.Marshal(models.StreamingAgentRunWithEventsRequest{…})` of `request_json`.
fn payload(request_json: &str) -> Bytes {
    Bytes::from(
        json!({
            "class_method": "streaming_agent_run_with_events",
            "input": {"request_json": request_json},
        })
        .to_string(),
    )
}

fn request_json(message: &str, session_id: &str) -> String {
    format!(
        r#"{{"message":{{"role":"user","parts":[{{"text":"{message}"}}]}},"session_id":"{session_id}","user_id":"{USER_ID}"}}"#
    )
}

/// Go: `h.streamJSONL(ctx, w, payload)` then `json.Unmarshal(w.sb.String(), &got)` (one line).
async fn stream(
    h: &StreamingAgentRunWithEventsHandler,
    payload: Bytes,
) -> (String, AgentSpaceStreamResponse) {
    let resp = h
        .stream_jsonl(payload)
        .await
        .unwrap_or_else(|e| panic!("streamJSONL() failed: {e}"));
    let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let out = String::from_utf8(body.to_vec()).unwrap();
    let got: AgentSpaceStreamResponse = serde_json::from_str(&out)
        .unwrap_or_else(|e| panic!("json.Unmarshal() failed: {e}\n{out}"));
    (out, got)
}

fn handler(cfg: AgentEngineConfig) -> StreamingAgentRunWithEventsHandler {
    StreamingAgentRunWithEventsHandler::new(
        cfg,
        APP_NAME,
        "streaming_agent_run_with_events",
        "async_stream",
    )
}

#[test]
fn test_decode_streaming_agent_run_with_events_request() {
    let payload = br#"{
		"class_method": "streaming_agent_run_with_events",
		"input": {
			"request_json": "{\"message\":{\"role\":\"user\",\"parts\":[{\"text\":\"Hi\"}]},\"session_id\":\"projects/111111111111/locations/global/collections/default_collection/engines/test-engine/sessions/12345678901234567890\",\"user_id\":\"test-user@example.com\"}"
		}
	}"#;
    let req: StreamingAgentRunWithEventsRequest = go_unmarshal(
        payload,
        &encode_shapes::MODELS_STREAMING_AGENT_RUN_WITH_EVENTS_REQUEST,
    )
    .unwrap_or_else(|e| panic!("json.Unmarshal() failed: {e}"));

    let (got, requested_session_id) = decode_streaming_agent_run_with_events_request(&req)
        .unwrap_or_else(|e| panic!("decodeStreamingAgentRunWithEventsRequest() failed: {e}"));

    let want = StreamingAgentRunWithEventsRunRequest {
        user_id: "test-user@example.com".into(),
        session_id: EXTERNAL_SESSION_ID.into(),
        message: Content {
            role: "user".into(),
            parts: vec![Part {
                text: "Hi".into(),
                ..Default::default()
            }],
        },
    };
    assert_eq!(
        got, want,
        "decodeStreamingAgentRunWithEventsRequest() mismatch"
    );
    assert_eq!(requested_session_id, want.session_id, "requestedSessionID");
}

// parity: adapted: AgentEngineConfig requires an agent loader (Go's config has none); a bare root agent is loaded and never run
#[tokio::test]
async fn test_ensure_backend_session_create_backend_session() {
    let cfg = config(single_agent_loader("root"));
    let session_service = Arc::clone(&cfg.session_service);
    let handler = handler(cfg);
    let mut req = StreamingAgentRunWithEventsRunRequest {
        user_id: USER_ID.into(),
        session_id: EXTERNAL_SESSION_ID.into(),
        ..Default::default()
    };
    let requested_session_id = req.session_id.clone();

    handler
        .ensure_backend_session(&mut req, &requested_session_id)
        .await
        .unwrap_or_else(|e| panic!("ensureBackendSession() failed: {e}"));
    assert!(
        !req.session_id.is_empty() && req.session_id != requested_session_id,
        "SessionID = {:?}, want generated backend session ID",
        req.session_id
    );

    let got = session_service
        .get(GetRequest {
            app_name: APP_NAME.into(),
            user_id: USER_ID.into(),
            session_id: req.session_id.clone(),
            ..Default::default()
        })
        .await
        .unwrap_or_else(|e| panic!("Get() failed: {e}"));
    assert_eq!(got.id(), req.session_id, "stored SessionID");
}

// parity: adapted: AgentEngineConfig requires an agent loader (Go's config has none); a bare root agent is loaded and never run
#[tokio::test]
async fn test_ensure_backend_session_reuse_returned_backend_session() {
    let cfg = config(single_agent_loader("root"));
    let created = cfg
        .session_service
        .create(CreateRequest {
            app_name: APP_NAME.into(),
            user_id: "jan@example.com".into(),
            ..Default::default()
        })
        .await
        .unwrap_or_else(|e| panic!("Create() failed: {e}"));

    let handler = handler(cfg);
    let mut req = StreamingAgentRunWithEventsRunRequest {
        user_id: "jan@example.com".into(),
        session_id: created.id().to_owned(),
        ..Default::default()
    };
    let requested = req.session_id.clone();

    handler
        .ensure_backend_session(&mut req, &requested)
        .await
        .unwrap_or_else(|e| panic!("ensureBackendSession() failed: {e}"));
    assert_eq!(
        req.session_id,
        created.id(),
        "SessionID, want existing backend session"
    );
}

#[tokio::test]
async fn test_stream_jsonl_agent_space_response_envelope() {
    let h = handler(config(echo_loader()));

    let (_, got) = stream(&h, payload(&request_json("Please", EXTERNAL_SESSION_ID))).await;
    assert!(
        !got.session_id.is_empty() && got.session_id != EXTERNAL_SESSION_ID,
        "SessionID = {:?}, want generated backend session ID",
        got.session_id
    );
    assert_eq!(got.events.len(), 1, "len(Events)");
    assert_eq!(
        got.events[0].content,
        Some(Content::from_text("Please", "user")),
        "event content mismatch"
    );
}

#[tokio::test]
async fn test_stream_jsonl_agent_space_accepts_returned_backend_session_id() {
    let cfg = config(echo_loader());
    let session_service = Arc::clone(&cfg.session_service);
    let h = handler(cfg);

    let (_, first) = stream(&h, payload(&request_json("Hi", EXTERNAL_SESSION_ID))).await;
    let (_, second) = stream(&h, payload(&request_json("Again", &first.session_id))).await;

    assert!(
        !first.session_id.is_empty() && first.session_id != EXTERNAL_SESSION_ID,
        "first SessionID = {:?}, want generated backend session ID",
        first.session_id
    );
    assert_eq!(
        second.session_id, first.session_id,
        "second SessionID, want returned backend session ID"
    );

    let list = session_service
        .list(ListRequest {
            app_name: APP_NAME.into(),
            user_id: USER_ID.into(),
        })
        .await
        .unwrap_or_else(|e| panic!("List() failed: {e}"));
    assert_eq!(list.len(), 1, "len(Sessions)");
}

#[tokio::test]
async fn test_stream_jsonl_agent_space_uses_non_streaming_mode() {
    let agent = gadk_engine::llm_agent::new(LlmAgentConfig {
        name: "StreamAware".into(),
        model: Some(Arc::new(StreamAwareLlm)),
        ..Default::default()
    })
    .unwrap_or_else(|e| panic!("failed to create agent: {e}"));
    let h = handler(config(Arc::new(SingleLoader::new(agent))));

    let (_, got) = stream(
        &h,
        payload(&request_json(
            "What is your capabilities",
            EXTERNAL_SESSION_ID,
        )),
    )
    .await;
    assert_eq!(got.events.len(), 1, "len(Events)");
    assert_eq!(
        got.events[0].content,
        Some(Content::from_text("final response", "model")),
        "event content mismatch"
    );
}

// parity: adapted: AgentEngineConfig cannot be nil (Go passes a nil config), so a config with a bare root agent is passed; Metadata is infallible in Rust
#[test]
fn test_streaming_agent_run_with_events_handler_metadata() {
    let handler = StreamingAgentRunWithEventsHandler::new(
        config(single_agent_loader("root")),
        "",
        "streaming_agent_run_with_events",
        "async_stream",
    );

    let mut got = handler.metadata();
    // cmpopts.IgnoreMapEntries: description
    got.remove("description");

    let want = json!({
        "api_mode": "async_stream",
        "name": "streaming_agent_run_with_events",
        "parameters": {
            "properties": {
                "request_json": {
                    "type": "string",
                },
            },
            "required": ["request_json"],
            "type": "object",
        },
    });
    assert_eq!(Value::Object(got), want, "Metadata() mismatch");
}

/// Go `failingSummarizer`: reports a compaction failure the way a summarizer whose model call
/// was rejected does.
struct FailingSummarizer;

#[async_trait::async_trait]
impl Summarizer for FailingSummarizer {
    async fn summarize_events(
        &self,
        _events: &[Event],
    ) -> Result<SummarizeResult, Box<dyn std::error::Error + Send + Sync>> {
        Err("the summarizer model refused the request".into())
    }
}

/// Go: Agent Engine treats a compaction failure the way the other three serving surfaces do.
#[tokio::test]
async fn test_stream_jsonl_compaction_failure_does_not_fail_the_turn() {
    let mut cfg = config(echo_loader());
    cfg.compaction = Some(CompactionConfig {
        compaction_interval: 1,
        summarizer: Some(Arc::new(FailingSummarizer)),
        ..Default::default()
    });
    let h = handler(cfg);

    let (out, got) = stream(&h, payload(&request_json("Please", EXTERNAL_SESSION_ID))).await;
    assert!(
        !out.contains("summarizer model refused"),
        "a compaction failure was reported to the client:\n{out}"
    );
    assert_eq!(
        got.events.len(),
        1,
        "len(Events), want the agent's answer to survive the compaction failure"
    );
    assert_eq!(
        got.events[0].content,
        Some(Content::from_text("Please", "user")),
        "event content mismatch"
    );
}
