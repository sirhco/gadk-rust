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
//! Public-path check of `gadk_engine::llm::run_llm_agent_as_node` (Go
//! `llmagent.RunLLMAgentAsNode`, agent/llmagent/llm_agent_wrapper.go): non-LLM agents are
//! rejected with Go's text, and a single_turn agent seeds the node input, yields the reply as the
//! node output (`message_as_output`) and writes the output key.

use std::any::Any;
use std::sync::Arc;

use futures::StreamExt;
use futures::stream::BoxStream;
use gadk_agent::{AgentConfig, Context, Mode};
use gadk_engine::llm::LlmAgentConfig;
use gadk_engine::llm::agent as llm_agent;
use gadk_engine::llm::run_llm_agent_as_node;
use gadk_genai::{Content, ROLE_MODEL};
use gadk_model::{Llm, LlmRequest, LlmResponse, ModelError};
use serde_json::{Value, json};

struct Fake;
impl Llm for Fake {
    fn name(&self) -> &str {
        "fake"
    }
    fn generate_content(
        &self,
        _req: LlmRequest,
        _stream: bool,
    ) -> BoxStream<'static, Result<LlmResponse, ModelError>> {
        let out = LlmResponse {
            content: Some(Content::from_text("answer", ROLE_MODEL)),
            ..Default::default()
        };
        futures::stream::once(async move { Ok(out) }).boxed()
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[tokio::test]
async fn rejects_non_llm_agent() {
    let a = gadk_agent::agent::new(AgentConfig {
        name: "custom".into(),
        ..Default::default()
    })
    .unwrap();
    let mut s = run_llm_agent_as_node(Context::builder().build(), a, Value::Null);
    let err = s.next().await.unwrap().unwrap_err();
    assert_eq!(
        err.to_string(),
        "RunLLMAgentAsNode: \"custom\" is not an LlmAgent"
    );
}

#[tokio::test]
async fn single_turn_yields_output_event_and_state_delta() {
    let a = llm_agent::new(LlmAgentConfig {
        name: "worker".into(),
        model: Some(Arc::new(Fake)),
        mode: Mode::SingleTurn,
        output_key: "out".into(),
        ..Default::default()
    })
    .unwrap();
    let ctx = Context::builder().agent(a.clone()).build();
    let mut s = run_llm_agent_as_node(ctx, a, json!("question"));
    let mut events = Vec::new();
    while let Some(e) = s.next().await {
        events.push(e.unwrap());
    }
    let last = events.last().unwrap();
    assert_eq!(last.output, Some(json!("answer")));
    assert!(last.node_info.as_ref().unwrap().message_as_output);
    assert_eq!(
        last.actions.state_delta.as_ref().unwrap().get("out"),
        Some(&json!("answer"))
    );
    // With Context::context_path (available once the cancel-cause work lands), T9/ports should
    // also assert the callback contexts see the promoted node path.
}
