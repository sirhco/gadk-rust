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

//! Rust-only: drives a `Runner::run_live` with a function tool through the public
//! `testutil::live` fakes, as the agentanalytics live ports (Go `newTestRunner(…).RunLive` over
//! `liveTestLLM` + `newFakeLiveServer`) do from outside the crate.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use gadk_agent::{Context, LiveRunConfig, ToolError};
use gadk_engine::llm::{LlmAgentConfig, agent as llm_agent};
use gadk_engine::runner::{PluginConfig, RunOptions, Runner, RunnerConfig};
use gadk_engine::testutil::live::{go_live_test_llm, model_turn, tool_call};
use gadk_engine::tool::{FunctionTool, FunctionToolConfig};
use gadk_model::Llm;
use gadk_platform::Platform;
use gadk_session::InMemorySessionService;
use serde::{Deserialize, Serialize};

#[derive(Deserialize, schemars::JsonSchema)]
struct NoArgs {}

#[derive(Serialize, schemars::JsonSchema)]
struct ToolOut {
    ok: bool,
}

#[tokio::test]
async fn run_live_with_a_tool_over_the_scripted_live_model() {
    // Go: newFakeLiveServer(t, `{"toolCall":…"run_helper"…}`, `{"serverContent":…"done"…}`).
    let model = go_live_test_llm(vec![
        tool_call(&[("call-1", "run_helper")]),
        model_turn("done", true),
    ]);
    let run_helper = FunctionTool::new(
        FunctionToolConfig::new("run_helper", "Runs helper"),
        |_ctx: Context, _a: NoArgs| async move { Ok::<_, ToolError>(ToolOut { ok: true }) },
    )
    .unwrap();
    let root = llm_agent::new(LlmAgentConfig {
        name: "live_root".into(),
        model: Some(model.clone() as Arc<dyn Llm>),
        tools: vec![run_helper],
        ..Default::default()
    })
    .unwrap();
    let runner = Runner::new(RunnerConfig {
        app_name: "app".into(),
        agent: root,
        session_service: Arc::new(InMemorySessionService::new()),
        artifact_service: None,
        memory_service: None,
        plugin_config: PluginConfig::default(),
        auto_create_session: true,
        platform: Platform::default(),
        compaction: None,
    })
    .unwrap();

    let (sess, mut events) = runner
        .run_live("u1", "s1", LiveRunConfig::default(), RunOptions::default())
        .await
        .unwrap();
    let mut saw_response = false;
    loop {
        let ev = tokio::time::timeout(Duration::from_secs(5), events.next())
            .await
            .expect("timed out waiting for the model's final turn")
            .expect("stream ended before the model's final turn")
            .unwrap();
        let Some(content) = &ev.response.content else {
            continue;
        };
        if content.parts.iter().any(|p| {
            p.function_response
                .as_ref()
                .is_some_and(|r| r.id == "call-1" && r.name == "run_helper")
        }) {
            saw_response = true;
        }
        if ev.response.turn_complete {
            assert_eq!(content.parts[0].text, "done");
            break;
        }
    }
    assert!(saw_response, "no function response event for call-1");
    // The flow sent the tool's response back to the model.
    assert!(model.sent_contents().iter().any(|c| {
        c.parts.iter().any(|p| {
            p.function_response
                .as_ref()
                .is_some_and(|r| r.id == "call-1")
        })
    }));

    // Go liveSess.Close(): the stream ends without an error.
    sess.close().await.unwrap();
    while let Some(item) = tokio::time::timeout(Duration::from_secs(5), events.next())
        .await
        .expect("the live stream did not end")
    {
        item.unwrap();
    }
}
