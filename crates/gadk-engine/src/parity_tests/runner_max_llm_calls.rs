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

//! Port of Go runner/max_llm_calls_test.go (package runner, internal).

use std::any::Any;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};

use futures::StreamExt;
use futures::stream::BoxStream;
use gadk_agent::{
    Agent, AgentConfig, AgentError, Context, EventStream, InvocationContext, RunConfig, Tool,
    ToolError, new_agent,
};
use gadk_genai::{Content, Part};
use gadk_model::{Llm, LlmRequest, LlmResponse, ModelError};
use gadk_platform::Platform;
use gadk_session::InMemorySessionService;
use serde::{Deserialize, Serialize};
use serde_json::Map;

use gadk_agent::run_config::run_limits::resolve_max_llm_calls_with;

use crate::agents::loop_agent::{self, LoopAgentConfig};
use crate::llm::{LlmAgentConfig, agent as llm_agent};
use crate::runner::{PluginConfig, RunOptions, Runner, RunnerConfig};
use crate::tool::{FunctionTool, FunctionToolConfig};

/// Go loopingModel: every turn asks for the `noop` tool again.
#[derive(Default)]
struct LoopingModel {
    calls: AtomicUsize,
}

impl Llm for LoopingModel {
    fn name(&self) -> &str {
        "looping-mock"
    }
    fn generate_content(
        &self,
        _req: LlmRequest,
        _stream: bool,
    ) -> BoxStream<'static, Result<LlmResponse, ModelError>> {
        self.calls.fetch_add(1, SeqCst);
        let mut part = Part::from_function_call("noop", Map::new());
        if let Some(fc) = part.function_call.as_mut() {
            fc.id = "fc".into();
        }
        let resp = LlmResponse {
            content: Some(Content::from_parts(vec![part], "model")),
            ..Default::default()
        };
        futures::stream::once(async move { Ok(resp) }).boxed()
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[derive(Deserialize, Serialize, schemars::JsonSchema)]
struct Empty {}

fn noop_tool() -> Arc<dyn Tool> {
    FunctionTool::new(
        FunctionToolConfig::new("noop", ""),
        |_ctx: Context, _in: Empty| async { Ok::<_, ToolError>(Empty {}) },
    )
    .unwrap()
}

fn looping_llm_agent(m: &Arc<LoopingModel>) -> Arc<dyn Agent> {
    llm_agent::new(LlmAgentConfig {
        name: "looper".into(),
        model: Some(m.clone()),
        tools: vec![noop_tool()],
        ..Default::default()
    })
    .unwrap()
}

fn runner_for(agent: Arc<dyn Agent>) -> Runner {
    Runner::new(RunnerConfig {
        app_name: "testApp".into(),
        agent,
        session_service: Arc::new(InMemorySessionService::new()),
        artifact_service: None,
        memory_service: None,
        plugin_config: PluginConfig::default(),
        auto_create_session: true,
        platform: Platform::default(),
        compaction: None,
    })
    .unwrap()
}

fn go() -> Content {
    Content::from_text("go", "user")
}

/// Port of Go runLoopingAgent: consume until the first error.
async fn run_looping_agent(cfg: RunConfig) -> (usize, Option<AgentError>) {
    let m = Arc::new(LoopingModel::default());
    let r = runner_for(looping_llm_agent(&m));
    let mut s = r.run("u", "s", Some(go()), cfg, RunOptions::default());
    let mut err = None;
    while let Some(item) = s.next().await {
        if let Err(e) = item {
            err = Some(e);
            break;
        }
    }
    (m.calls.load(SeqCst), err)
}

#[tokio::test]
async fn test_run_max_llm_calls_bounds_the_run() {
    let (calls, err) = run_looping_agent(RunConfig {
        max_llm_calls: 3,
        ..Default::default()
    })
    .await;
    assert!(
        matches!(err, Some(AgentError::LlmCallsLimitExceeded { .. })),
        "Run() error = {err:?}, want LlmCallsLimitExceeded"
    );
    // The budget is spent, then the next call is refused: the model is reached exactly 3 times.
    assert_eq!(calls, 3);
}

// parity: adapted: Go sets ADK_MAX_LLM_CALLS=2 with t.Setenv; std::env::set_var is unsafe in edition 2024 (unsafe_code is forbidden), so the env value is fed to resolve_max_llm_calls_with and the run uses the budget it resolves
#[tokio::test]
async fn test_run_max_llm_calls_defaults_to_a_budget() {
    // An empty RunConfig must not mean "unlimited". Go keeps the budget small via the environment.
    let budget = resolve_max_llm_calls_with(RunConfig::default().max_llm_calls, Some("2"));
    assert_eq!(budget, 2, "resolve_max_llm_calls_with(0, Some(\"2\"))");

    let (calls, err) = run_looping_agent(RunConfig {
        max_llm_calls: budget,
        ..Default::default()
    })
    .await;

    assert!(
        matches!(err, Some(AgentError::LlmCallsLimitExceeded { .. })),
        "Run() error = {err:?}, want it to wrap ErrLLMCallsLimitExceeded"
    );
    assert_eq!(calls, 2, "model calls");
}

#[tokio::test]
async fn test_run_max_llm_calls_negative_means_unlimited() {
    // Cannot assert "runs forever", so assert the limit is not what stops it: the consumer stops
    // after `STOP_AFTER` calls and the error (if any) must not be the budget.
    let m = Arc::new(LoopingModel::default());
    let r = runner_for(looping_llm_agent(&m));
    const STOP_AFTER: usize = 12;
    let mut got_err = None;
    {
        let mut s = r.run(
            "u",
            "s",
            Some(go()),
            RunConfig {
                max_llm_calls: -1,
                ..Default::default()
            },
            RunOptions::default(),
        );
        while let Some(item) = s.next().await {
            if let Err(e) = item {
                got_err = Some(e);
                break;
            }
            if m.calls.load(SeqCst) >= STOP_AFTER {
                break;
            }
        }
    }

    assert!(
        !matches!(got_err, Some(AgentError::LlmCallsLimitExceeded { .. })),
        "Run() was stopped by the budget with MaxLLMCalls = -1"
    );
    let calls = m.calls.load(SeqCst);
    assert!(
        calls >= STOP_AFTER,
        "model calls = {calls}, want at least {STOP_AFTER} before the consumer stopped"
    );
}

/// Port of Go rangeWholeRun: consume a run the way the adkrest SSE handler does, ranging on after
/// an error. Stops once the model has been called far more often than the budget allows, or once
/// more than one limit error has arrived, so a regression fails rather than hangs.
async fn range_whole_run(root: Arc<dyn Agent>, m: &LoopingModel, cfg: RunConfig) -> usize {
    let r = runner_for(root);
    const GIVE_UP_AFTER_CALLS: usize = 50;
    let mut limit_errs = 0;
    let mut s = r.run("u", "s", Some(go()), cfg, RunOptions::default());
    while let Some(item) = s.next().await {
        if let Err(e) = item {
            if !matches!(e, AgentError::LlmCallsLimitExceeded { .. }) {
                panic!("Run() yielded error {e}, want only ErrLLMCallsLimitExceeded");
            }
            limit_errs += 1;
        }
        if limit_errs > 1 || m.calls.load(SeqCst) > GIVE_UP_AFTER_CALLS {
            break;
        }
    }
    limit_errs
}

#[tokio::test]
async fn test_run_max_llm_calls_ends_a_loop_agent_run() {
    // A loop agent root takes the agent path in Runner::run rather than the node path an LLM agent
    // root takes, so this also covers the budget being set on that path.
    let m = Arc::new(LoopingModel::default());
    let root = loop_agent::new(LoopAgentConfig {
        agent: AgentConfig {
            name: "loop".into(),
            sub_agents: vec![looping_llm_agent(&m)],
            ..Default::default()
        },
        max_iterations: 0,
    })
    .unwrap();

    let limit_errs = range_whole_run(
        root,
        &m,
        RunConfig {
            max_llm_calls: 3,
            ..Default::default()
        },
    )
    .await;

    assert_eq!(m.calls.load(SeqCst), 3, "model calls");
    assert_eq!(
        limit_errs, 1,
        "limit errors: want the run to end after exactly 1"
    );
}

#[tokio::test]
async fn test_run_max_llm_calls_ends_a_run_whose_root_ignores_errors() {
    // A custom root that runs its sub-agent again after an error, as the loop agent did before it
    // learned to stop. Every rerun is refused before it reaches the model, so without the runner
    // ending the run the consumer would get an endless stream of limit errors.
    let m = Arc::new(LoopingModel::default());
    let looper = looping_llm_agent(&m);
    let sub = looper.clone();
    let root = new_agent(AgentConfig {
        name: "stubborn".into(),
        sub_agents: vec![looper],
        run: Some(Arc::new(move |ctx: InvocationContext| -> EventStream {
            let looper = sub.clone();
            Box::pin(async_stream::stream! {
                loop {
                    let mut s = looper.run(ctx.clone());
                    while let Some(item) = s.next().await {
                        yield item;
                    }
                }
            })
        })),
        ..Default::default()
    })
    .unwrap();

    let limit_errs = range_whole_run(
        root,
        &m,
        RunConfig {
            max_llm_calls: 3,
            ..Default::default()
        },
    )
    .await;

    assert_eq!(m.calls.load(SeqCst), 3, "model calls");
    assert_eq!(
        limit_errs, 1,
        "limit errors: want the run to end after exactly 1"
    );
}
