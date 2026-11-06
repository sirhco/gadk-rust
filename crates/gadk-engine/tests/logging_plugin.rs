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
//! Rust-only tests for `logging_plugin` (Go plugin/loggingplugin has no tests).

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use gadk_agent::{Context, RunConfig, ToolError};
use gadk_engine::llm::{LlmAgentConfig, agent as llm_agent};
use gadk_engine::plugin::logging_plugin;
use gadk_engine::runner::{PluginConfig, RunOptions, Runner, RunnerConfig};
use gadk_engine::testutil::MockModel;
use gadk_engine::tool::{FunctionTool, FunctionToolConfig};
use gadk_genai::{Content, FinishReason, Part, ROLE_MODEL, ROLE_USER};
use gadk_model::{Llm, LlmResponse};
use gadk_platform::Platform;
use gadk_session::InMemorySessionService;
use serde::Deserialize;
use serde_json::{Value, json};

/// Installs (once per test binary) a no-op global registry so callsite interest never collapses
/// to "off" while thread-local capture subscribers come and go on parallel test threads (the
/// external-test equivalent of the lib's `pin_tracing_interest`).
fn pin_tracing_interest() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = tracing::subscriber::set_global_default(tracing_subscriber::registry());
    });
}

#[derive(Clone, Default)]
struct Buf(Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for Buf {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Buf {
    type Writer = Buf;
    fn make_writer(&'a self) -> Buf {
        self.clone()
    }
}

#[derive(Deserialize, schemars::JsonSchema)]
struct EchoArgs {
    text: String,
}

fn obj(v: Value) -> serde_json::Map<String, Value> {
    let Value::Object(m) = v else { unreachable!() };
    m
}

#[tokio::test(flavor = "current_thread")]
async fn logging_plugin_logs_the_invocation() {
    pin_tracing_interest();
    let buf = Buf::default();
    let sub = tracing_subscriber::fmt()
        .with_writer(buf.clone())
        .with_ansi(false)
        .without_time()
        .with_target(false)
        .with_max_level(tracing::Level::INFO)
        .finish();
    let _g = tracing::subscriber::set_default(sub);

    let echo = FunctionTool::new(
        FunctionToolConfig::new("echo", "echoes the text"),
        |_ctx: Context, a: EchoArgs| async move { Ok::<String, ToolError>(a.text) },
    )
    .unwrap();
    let call = Ok(LlmResponse {
        content: Some(Content::from_parts(
            vec![Part::from_function_call("echo", obj(json!({"text": "hi"})))],
            ROLE_MODEL,
        )),
        finish_reason: Some(FinishReason::Stop),
        ..Default::default()
    });
    let done = Ok(LlmResponse {
        content: Some(Content::from_text("done", ROLE_MODEL)),
        finish_reason: Some(FinishReason::Stop),
        ..Default::default()
    });
    let model: Arc<dyn Llm> = MockModel::new(vec![call, done]);
    let agent = llm_agent::new(LlmAgentConfig {
        name: "assistant".into(),
        model: Some(model),
        tools: vec![echo],
        ..Default::default()
    })
    .unwrap();
    let runner = Runner::new(RunnerConfig {
        app_name: "app".into(),
        agent,
        session_service: Arc::new(InMemorySessionService::new()),
        artifact_service: None,
        memory_service: None,
        plugin_config: PluginConfig {
            plugins: vec![logging_plugin()],
            close_timeout: Duration::from_secs(1),
        },
        auto_create_session: true,
        platform: Platform::default(),
    })
    .unwrap();
    let items: Vec<_> = runner
        .run(
            "u",
            "s",
            Some(Content::from_text("hello", ROLE_USER)),
            RunConfig::default(),
            RunOptions::default(),
        )
        .collect()
        .await;
    assert!(items.iter().all(|i| i.is_ok()), "{items:?}");

    let out = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
    let ordered = [
        "[logging_plugin] 🚀 USER MESSAGE RECEIVED",
        "[logging_plugin] 🏃 INVOCATION STARTING",
        "[logging_plugin] 🤖 AGENT STARTING",
        "[logging_plugin] 🧠 LLM REQUEST",
        "[logging_plugin] 🧠 LLM RESPONSE",
        "[logging_plugin] 🔧 TOOL STARTING",
        "[logging_plugin] 🔧 TOOL COMPLETED",
        "[logging_plugin] ✅ INVOCATION COMPLETED",
    ];
    let mut last = 0;
    for line in ordered {
        let pos = out[last..]
            .find(line)
            .unwrap_or_else(|| panic!("missing (in order) {line:?} in:\n{out}"));
        last += pos + line.len();
    }
    for line in [
        "[logging_plugin]    Root Agent: assistant",
        "[logging_plugin]    User Content: text: 'hello'",
        "[logging_plugin]    Model: mock",
        "[logging_plugin]    Available Tools: [echo]",
        "[logging_plugin]    Tool Name: echo",
        "[logging_plugin] 📢 EVENT YIELDED",
        "[logging_plugin] 🤖 AGENT COMPLETED",
    ] {
        assert!(out.contains(line), "missing {line:?} in:\n{out}");
    }
}

#[test]
fn logging_plugin_name_and_callbacks() {
    let p = logging_plugin();
    assert_eq!(p.name(), "logging_plugin");
    assert!(p.on_user_message_callback().is_some());
    assert!(p.before_run_callback().is_some());
    assert!(p.on_event_callback().is_some());
    assert!(p.after_run_callback().is_some());
    assert!(p.before_agent_callback().is_some());
    assert!(p.after_agent_callback().is_some());
    assert!(p.before_model_callback().is_some());
    assert!(p.after_model_callback().is_some());
    assert!(p.on_model_error_callback().is_some());
    assert!(p.before_tool_callback().is_some());
    assert!(p.after_tool_callback().is_some());
    assert!(p.on_tool_error_callback().is_some());
    // Go plugin.New installs a no-op close when none is given; Plugin::new does the same.
    assert!(p.close().is_some());
}
