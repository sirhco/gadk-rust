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
//! Port of Go agent/llmagent/llmagent_beforecallback_outputkey_test.go.

use std::any::Any;
use std::sync::Arc;

use futures::StreamExt;
use futures::stream::BoxStream;
use gadk_agent::{BeforeAgentCallback, Context, RunConfig};
use gadk_engine::llm::LlmAgentConfig;
use gadk_engine::llm::agent as llm_agent;
use gadk_engine::runner::{PluginConfig, RunOptions, Runner, RunnerConfig};
use gadk_genai::{Content, ROLE_MODEL, ROLE_USER};
use gadk_model::{Llm, LlmRequest, LlmResponse, ModelError};
use gadk_platform::Platform;
use gadk_session::{CreateRequest, Event, GetRequest, InMemorySessionService, SessionService};
use serde_json::{Map, Value, json};

/// Port of Go FakeLLM (state_agent_test.go) with no GenerateContentFunc: always answers
/// "fake model response".
struct FakeLlm;
impl Llm for FakeLlm {
    fn name(&self) -> &str {
        "fake-llm"
    }
    fn generate_content(
        &self,
        _req: LlmRequest,
        _stream: bool,
    ) -> BoxStream<'static, Result<LlmResponse, ModelError>> {
        let out = LlmResponse {
            content: Some(Content::from_text("fake model response", ROLE_MODEL)),
            ..Default::default()
        };
        futures::stream::once(async move { Ok(out) }).boxed()
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Port of Go runWithBeforeAgentCallback: one turn of an LlmAgent with the given output key and
/// before-agent callback; returns the yielded events and the session state read back from the
/// session service.
async fn run_with_before_agent_callback(
    output_key: &str,
    cb: BeforeAgentCallback,
) -> (Vec<Event>, Arc<dyn gadk_session::Session>) {
    let a = llm_agent::new(LlmAgentConfig {
        name: "a".into(),
        model: Some(Arc::new(FakeLlm)),
        output_key: output_key.into(),
        before_agent_callbacks: vec![cb],
        ..Default::default()
    })
    .unwrap();
    let svc: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    let r = Runner::new(RunnerConfig {
        app_name: "app".into(),
        agent: a,
        session_service: svc.clone(),
        artifact_service: None,
        memory_service: None,
        plugin_config: PluginConfig::default(),
        auto_create_session: false,
        platform: Platform::default(),
    })
    .unwrap();
    let cr = svc
        .create(CreateRequest {
            app_name: "app".into(),
            user_id: "u".into(),
            ..Default::default()
        })
        .await
        .unwrap();

    let mut events = Vec::new();
    let mut stream = r.run(
        "u",
        cr.id(),
        Some(Content::from_text("hi", ROLE_USER)),
        RunConfig::default(),
        RunOptions::default(),
    );
    while let Some(ev) = stream.next().await {
        events.push(ev.unwrap());
    }
    let got = svc
        .get(GetRequest {
            app_name: "app".into(),
            user_id: "u".into(),
            session_id: cr.id().into(),
            ..Default::default()
        })
        .await
        .unwrap();
    (events, got)
}

#[tokio::test]
async fn test_llm_agent_output_key_with_before_agent_callback() {
    const CACHED: &str = "cached answer";
    let short_circuit: BeforeAgentCallback = Arc::new(|_ctx: Context| {
        Box::pin(async { Ok(Some(Content::from_text(CACHED, ROLE_MODEL))) })
    });
    let pass_through: BeforeAgentCallback = Arc::new(|_ctx: Context| Box::pin(async { Ok(None) }));

    // (name, output key, callback, want state, want text)
    type Case = (
        &'static str,
        &'static str,
        BeforeAgentCallback,
        Vec<(&'static str, Value)>,
        &'static str,
    );
    let cases: Vec<Case> = vec![
        (
            "short-circuit content is saved under output_key",
            "result",
            short_circuit.clone(),
            vec![("result", json!(CACHED))],
            CACHED,
        ),
        (
            "short-circuit without output_key leaves state unchanged",
            "",
            short_circuit,
            vec![],
            CACHED,
        ),
        (
            "nil callback result runs the model and saves its response",
            "result",
            pass_through,
            vec![("result", json!("fake model response"))],
            "fake model response",
        ),
    ];
    for (name, output_key, cb, want_state, want_text) in cases {
        let (events, sess) = run_with_before_agent_callback(output_key, cb).await;
        let state = sess.state();

        assert_eq!(
            events.len(),
            1,
            "{name}: got {} events, want 1 (the state delta must ride on the response event)",
            events.len()
        );
        let ev = &events[0];
        let text = &ev.response.content.as_ref().expect("content").parts[0].text;
        assert_eq!(text, want_text, "{name}: event text");
        let delta: Map<String, Value> = ev.actions.state_delta.clone().unwrap_or_default();
        for (k, want) in &want_state {
            let got = state.get(k);
            assert!(
                got.as_ref().is_ok_and(|v| v == want),
                "{name}: state[{k:?}] = {got:?}, want {want}"
            );
            assert_eq!(delta.get(*k), Some(want), "{name}: event StateDelta[{k:?}]");
        }
        if want_state.is_empty() {
            assert!(
                state.get("result").is_err(),
                "{name}: state[result] set, want it unset"
            );
            assert!(
                delta.is_empty(),
                "{name}: event StateDelta has {} keys, want 0",
                delta.len()
            );
        }
    }
}
