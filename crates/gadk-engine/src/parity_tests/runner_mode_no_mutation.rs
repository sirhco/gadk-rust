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

//! Port of Go runner/mode_no_mutation_test.go.

use std::sync::Arc;

use futures::StreamExt;
use gadk_agent::{Mode, RunConfig};
use gadk_genai::{Content, FinishReason, ROLE_MODEL, ROLE_USER};
use gadk_model::{Llm, LlmResponse};
use gadk_platform::Platform;
use gadk_session::InMemorySessionService;

use crate::llm::state::reveal;
use crate::llm::{LlmAgentConfig, agent as llm_agent};
use crate::runner::{PluginConfig, RunOptions, Runner, RunnerConfig};
use crate::testutil::MockModel;

// parity: adapted: MockModel replaces Go's scriptedModel (same single text reply)
#[tokio::test]
async fn test_run_does_not_mutate_the_root_agents_mode() {
    let model: Arc<dyn Llm> = MockModel::new(vec![Ok(LlmResponse {
        content: Some(Content::from_text("reply 1", ROLE_MODEL)),
        finish_reason: Some(FinishReason::Stop),
        ..Default::default()
    })]);
    let root = llm_agent::new(LlmAgentConfig {
        name: "root".into(),
        model: Some(model),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(
        reveal(&*root).unwrap().cfg.mode,
        Mode::Unset,
        "precondition: declared mode"
    );
    let runner = Runner::new(RunnerConfig {
        app_name: "app".into(),
        agent: root.clone(),
        session_service: Arc::new(InMemorySessionService::new()),
        artifact_service: None,
        memory_service: None,
        plugin_config: PluginConfig::default(),
        auto_create_session: true,
        platform: Platform::default(),
        compaction: None,
    })
    .unwrap();
    let mut s = runner.run(
        "u",
        "s1",
        Some(Content::from_text("hi", ROLE_USER)),
        RunConfig::default(),
        RunOptions::default(),
    );
    let mut events = 0;
    while let Some(item) = s.next().await {
        item.unwrap();
        events += 1;
    }
    assert!(events > 0, "the run produced no events");
    assert_eq!(
        reveal(&*root).unwrap().cfg.mode,
        Mode::Unset,
        "a run must not mutate the agent"
    );
}
