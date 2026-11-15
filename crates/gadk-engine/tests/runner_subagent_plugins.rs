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
//! Port of ADK-Go runner/subagent_plugins_test.go.

use std::any::Any;
use std::sync::Arc;

use futures::StreamExt;
use futures::stream::BoxStream;
use gadk_agent::{
    AfterModelCallback, Agent, AgentConfig, AgentError, Context, EventStream, Plugin,
    PluginConfig as AgentPluginConfig, RunConfig, Tool, ToolError,
};
use gadk_engine::agents::sequential_agent::{self, SequentialAgentConfig};
use gadk_engine::llm::LlmAgentConfig;
use gadk_engine::llm::agent as llm_agent;
use gadk_engine::runner::{PluginConfig, RunOptions, Runner, RunnerConfig};
use gadk_engine::tool::{AgentTool, FunctionTool, FunctionToolConfig};
use gadk_genai::{Content, FunctionCall};
use gadk_model::{Llm, LlmRequest, LlmResponse, ModelError};
use gadk_platform::Platform;
use gadk_session::InMemorySessionService;
use parking_lot::Mutex;
use serde_json::{Value, json};

/// Port of Go `scriptedModel`: one response per call; past the end, repeats the last.
struct ScriptedModel {
    responses: Vec<Content>,
    call: Mutex<usize>,
}

impl ScriptedModel {
    fn arc(responses: Vec<Content>) -> Arc<dyn Llm> {
        Arc::new(Self {
            responses,
            call: Mutex::new(0),
        })
    }
}

impl Llm for ScriptedModel {
    fn name(&self) -> &str {
        "scripted"
    }
    fn generate_content(
        &self,
        _req: LlmRequest,
        _stream: bool,
    ) -> BoxStream<'static, Result<LlmResponse, ModelError>> {
        let mut call = self.call.lock();
        let i = (*call).min(self.responses.len() - 1);
        *call += 1;
        let resp = LlmResponse {
            content: Some(self.responses[i].clone()),
            ..Default::default()
        };
        futures::stream::iter([Ok(resp)]).boxed()
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn model_text(text: &str) -> Content {
    Content::from_text(text, "model")
}

/// Go `callContent`.
fn call_content(name: &str, args: Value) -> Content {
    Content::from_function_call(
        FunctionCall {
            name: name.into(),
            args: args.as_object().cloned().unwrap_or_default(),
            ..Default::default()
        },
        "model",
    )
}

fn runner_with(agent: Arc<dyn Agent>, app: &str, plugins: Vec<Plugin>) -> Runner {
    Runner::new(RunnerConfig {
        app_name: app.into(),
        agent,
        session_service: Arc::new(InMemorySessionService::new()),
        artifact_service: None,
        memory_service: None,
        plugin_config: PluginConfig {
            plugins,
            ..Default::default()
        },
        auto_create_session: true,
        platform: Platform::default(),
        compaction: None,
    })
    .expect("runner.New()")
}

fn run(r: &Runner, user: &str, session: &str, text: &str) -> EventStream {
    r.run(
        user,
        session,
        Some(Content::from_text(text, "user")),
        RunConfig::default(),
        RunOptions::default(),
    )
}

async fn drain_ok(s: EventStream) {
    futures::TryStreamExt::try_collect::<Vec<_>>(s)
        .await
        .expect("Run() error");
}

/// Go `pluginRecorder`: records which of its callbacks fired.
#[derive(Default)]
struct Recorded {
    before_tools: Vec<String>,
    before_agents: Vec<String>,
    before_models: Vec<String>,
    before_runs: usize,
    user_msgs: Vec<String>,
}

fn recorder_plugin(rec: &Arc<Mutex<Recorded>>) -> Plugin {
    let (r1, r2, r3, r4, r5) = (
        rec.clone(),
        rec.clone(),
        rec.clone(),
        rec.clone(),
        rec.clone(),
    );
    Plugin::new(AgentPluginConfig {
        name: "recorder".into(),
        before_tool_callback: Some(Arc::new(move |_ctx, tl: Arc<dyn Tool>, _args| {
            r1.lock().before_tools.push(tl.name().to_string());
            Box::pin(async { Ok(None) })
        })),
        before_agent_callback: Some(Arc::new(move |ctx: Context| {
            r2.lock().before_agents.push(ctx.agent_name());
            Box::pin(async { Ok(None) })
        })),
        before_model_callback: Some(Arc::new(move |ctx: Context, req| {
            r3.lock().before_models.push(ctx.agent_name());
            Box::pin(async move { (req, Ok(None)) })
        })),
        before_run_callback: Some(Arc::new(move |_ctx| {
            r4.lock().before_runs += 1;
            Box::pin(async { Ok(None) })
        })),
        on_user_message_callback: Some(Arc::new(move |_ctx, msg: Content| {
            r5.lock().user_msgs.push(msg.parts[0].text.clone());
            Box::pin(async { Ok(None) })
        })),
        ..Default::default()
    })
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct NoArgs {}

/// Go `newInnerTool`.
fn new_inner_tool() -> Arc<dyn Tool> {
    FunctionTool::new(
        FunctionToolConfig::new("inner_tool", "returns pong"),
        |_ctx: Context, _a: NoArgs| async { Ok::<_, ToolError>("pong".to_string()) },
    )
    .expect("functiontool.New()")
}

/// Go `runParentWithSubAgent`: a parent LlmAgent that calls `sub` through agenttool, with the
/// recorder registered on the parent runner only.
async fn run_parent_with_sub_agent(sub: Arc<dyn Agent>, rec: &Arc<Mutex<Recorded>>) {
    let parent = llm_agent::new(LlmAgentConfig {
        name: "parent".into(),
        model: Some(ScriptedModel::arc(vec![
            call_content(sub.name(), json!({"request": "delegated prompt"})),
            model_text("all done"),
        ])),
        tools: vec![AgentTool::new(sub, None)],
        ..Default::default()
    })
    .expect("llmagent.New()");
    let r = runner_with(parent, "plugins_app", vec![recorder_plugin(rec)]);
    drain_ok(run(&r, "u", "s", "go")).await;
}

#[tokio::test]
#[ignore = "8a bug: AgentTool (tool/agent_tool.rs) still starts its sub-runner with Runner::run; Go #1777 starts it with run_with_context(&ctx.with_inherited_plugins(), …) — un-ignore with that change"]
async fn test_runner_parent_plugins_reach_agent_tool_llm_sub_agent() {
    let sub = llm_agent::new(LlmAgentConfig {
        name: "sub_agent".into(),
        description: "does the work".into(),
        model: Some(ScriptedModel::arc(vec![
            call_content("inner_tool", Value::Null),
            model_text("leaf done"),
        ])),
        tools: vec![new_inner_tool()],
        ..Default::default()
    })
    .unwrap();
    let rec = Arc::new(Mutex::new(Recorded::default()));
    run_parent_with_sub_agent(sub, &rec).await;
    let rec = rec.lock();
    assert_eq!(
        rec.before_tools,
        ["sub_agent", "inner_tool"],
        "BeforeToolCallback"
    );
    // The parent's agent and model callbacks fire for the sub-agent too.
    assert_eq!(
        rec.before_agents,
        ["parent", "sub_agent"],
        "BeforeAgentCallback"
    );
    assert!(
        rec.before_models.iter().any(|a| a == "sub_agent"),
        "BeforeModelCallback saw {:?}, want it to include sub_agent",
        rec.before_models
    );
}

#[tokio::test]
#[ignore = "8a bug: AgentTool (tool/agent_tool.rs) still starts its sub-runner with Runner::run; Go #1777 starts it with run_with_context(&ctx.with_inherited_plugins(), …) — un-ignore with that change"]
async fn test_runner_parent_plugins_reach_agent_tool_sequential_sub_agent() {
    let leaf = llm_agent::new(LlmAgentConfig {
        name: "leaf".into(),
        model: Some(ScriptedModel::arc(vec![
            call_content("inner_tool", Value::Null),
            model_text("leaf done"),
        ])),
        tools: vec![new_inner_tool()],
        ..Default::default()
    })
    .unwrap();
    let seq = sequential_agent::new(SequentialAgentConfig {
        agent: AgentConfig {
            name: "seq_agent".into(),
            description: "wraps leaf".into(),
            sub_agents: vec![leaf],
            ..Default::default()
        },
    })
    .unwrap();
    let rec = Arc::new(Mutex::new(Recorded::default()));
    run_parent_with_sub_agent(seq, &rec).await;
    assert_eq!(
        rec.lock().before_tools,
        ["seq_agent", "inner_tool"],
        "BeforeToolCallback"
    );
}

#[tokio::test]
async fn test_runner_parent_plugins_do_not_own_sub_run_callbacks() {
    let sub = llm_agent::new(LlmAgentConfig {
        name: "sub_agent".into(),
        description: "does the work".into(),
        model: Some(ScriptedModel::arc(vec![model_text("leaf done")])),
        ..Default::default()
    })
    .unwrap();
    let rec = Arc::new(Mutex::new(Recorded::default()));
    run_parent_with_sub_agent(sub, &rec).await;
    let rec = rec.lock();
    assert_eq!(rec.user_msgs, ["go"], "OnUserMessageCallback");
    assert_eq!(
        rec.before_runs, 1,
        "BeforeRunCallback runs (the parent run only)"
    );
}

/// Go's llm_judge plugin: its AfterModelCallback records the agent name and runs `judge` through
/// its own plugin-less runner, failing once it has re-entered more than `limit` times.
fn judge_plugin(judge: Arc<Runner>, seen: Arc<Mutex<Vec<String>>>, limit: usize) -> Plugin {
    let cb: AfterModelCallback = Arc::new(move |ctx: Context, resp: LlmResponse, _err| {
        let (judge, seen) = (judge.clone(), seen.clone());
        Box::pin(async move {
            let n = {
                let mut seen = seen.lock();
                seen.push(ctx.agent_name());
                seen.len()
            };
            if n > limit {
                return (
                    resp,
                    Err(AgentError::other(format!(
                        "judge callback re-entered {n} times"
                    ))),
                );
            }
            // Go passes the callback's ctx to the judge runner.
            let mut s = judge.run_with_context(
                &ctx,
                "judge-user",
                &format!("judge-{n}"),
                Some(Content::from_text("is this safe?", "user")),
                RunConfig::default(),
                RunOptions::default(),
            );
            while let Some(item) = s.next().await {
                if let Err(e) = item {
                    return (resp, Err(e));
                }
            }
            (resp, Ok(None))
        })
    });
    Plugin::new(AgentPluginConfig {
        name: "llm_judge".into(),
        after_model_callback: Some(cb),
        ..Default::default()
    })
}

fn judge_runner() -> Arc<Runner> {
    let judge = llm_agent::new(LlmAgentConfig {
        name: "judge".into(),
        model: Some(ScriptedModel::arc(vec![model_text("SAFE")])),
        ..Default::default()
    })
    .unwrap();
    Arc::new(runner_with(judge, "judge_app", vec![]))
}

#[tokio::test]
async fn test_runner_plugin_callback_runner_does_not_inherit_plugins() {
    let seen: Arc<Mutex<Vec<String>>> = Arc::default();
    let pl = judge_plugin(judge_runner(), seen.clone(), 3);
    let parent = llm_agent::new(LlmAgentConfig {
        name: "parent".into(),
        model: Some(ScriptedModel::arc(vec![model_text("hello")])),
        ..Default::default()
    })
    .unwrap();
    let r = runner_with(parent, "app", vec![pl]);
    drain_ok(run(&r, "u", "s", "go")).await;
    assert_eq!(
        *seen.lock(),
        ["parent"],
        "AfterModelCallback (the judge runner must not inherit the plugin)"
    );
}

#[tokio::test]
async fn test_runner_separate_runner_does_not_inherit_context_plugins() {
    let rec = Arc::new(Mutex::new(Recorded::default()));
    let separate_agent = llm_agent::new(LlmAgentConfig {
        name: "separate".into(),
        model: Some(ScriptedModel::arc(vec![model_text("separate done")])),
        ..Default::default()
    })
    .unwrap();
    let separate = Arc::new(runner_with(separate_agent, "separate_app", vec![]));
    let call_separate = FunctionTool::new(
        FunctionToolConfig::new("call_separate", "runs another runner"),
        move |ctx: Context, _a: NoArgs| {
            let separate = separate.clone();
            async move {
                let mut s = separate.run_with_context(
                    &ctx,
                    "u2",
                    "s2",
                    Some(Content::from_text("hi", "user")),
                    RunConfig::default(),
                    RunOptions::default(),
                );
                while let Some(item) = s.next().await {
                    item.map_err(|e| ToolError::Execution(Box::new(e)))?;
                }
                Ok::<_, ToolError>("ok".to_string())
            }
        },
    )
    .unwrap();
    let parent = llm_agent::new(LlmAgentConfig {
        name: "parent".into(),
        model: Some(ScriptedModel::arc(vec![
            call_content("call_separate", Value::Null),
            model_text("done"),
        ])),
        tools: vec![call_separate],
        ..Default::default()
    })
    .unwrap();
    let r = runner_with(parent, "app", vec![recorder_plugin(&rec)]);
    drain_ok(run(&r, "u", "s", "go")).await;
    let rec = rec.lock();
    assert!(
        !rec.before_agents.iter().any(|a| a == "separate")
            && !rec.before_models.iter().any(|a| a == "separate"),
        "parent plugin fired for the separate runner: agents {:?}, models {:?}",
        rec.before_agents,
        rec.before_models
    );
}

#[tokio::test]
async fn test_runner_judge_inside_agent_tool_sub_run_does_not_inherit_plugins() {
    let seen: Arc<Mutex<Vec<String>>> = Arc::default();
    let pl = judge_plugin(judge_runner(), seen.clone(), 5);
    let sub = llm_agent::new(LlmAgentConfig {
        name: "sub_agent".into(),
        description: "does the work".into(),
        model: Some(ScriptedModel::arc(vec![model_text("leaf done")])),
        ..Default::default()
    })
    .unwrap();
    let parent = llm_agent::new(LlmAgentConfig {
        name: "parent".into(),
        model: Some(ScriptedModel::arc(vec![
            call_content("sub_agent", json!({"request": "delegated prompt"})),
            model_text("all done"),
        ])),
        tools: vec![AgentTool::new(sub, None)],
        ..Default::default()
    })
    .unwrap();
    let r = runner_with(parent, "app", vec![pl]);
    drain_ok(run(&r, "u", "s", "go")).await;
    let seen = seen.lock();
    assert!(
        !seen.iter().any(|a| a == "judge"),
        "AfterModelCallback saw {seen:?}, want no judge entries"
    );
}

/// gadk-only guard of the runner side of #1777 (Go `withPluginManager`), independent of the agent
/// tool: a tool that starts a runner with a context it marked (`with_inherited_plugins`) gets the
/// calling run's plugins in that runner when it has none of its own; a runner with its own
/// plugins keeps them, and the run-level callbacks stay with the parent run.
#[tokio::test]
async fn run_with_context_inherits_only_a_marked_context_s_plugins() {
    for own_plugins in [false, true] {
        let rec = Arc::new(Mutex::new(Recorded::default()));
        let own = Arc::new(Mutex::new(Recorded::default()));
        let leaf = llm_agent::new(LlmAgentConfig {
            name: "leaf".into(),
            model: Some(ScriptedModel::arc(vec![model_text("leaf done")])),
            ..Default::default()
        })
        .unwrap();
        let sub_plugins = if own_plugins {
            vec![recorder_plugin(&own)]
        } else {
            vec![]
        };
        let sub = Arc::new(runner_with(leaf, "sub_app", sub_plugins));
        let call_sub = FunctionTool::new(
            FunctionToolConfig::new("call_sub", "runs the sub-runner like agenttool"),
            move |ctx: Context, _a: NoArgs| {
                let sub = sub.clone();
                async move {
                    let mut s = sub.run_with_context(
                        &ctx.with_inherited_plugins(),
                        "u2",
                        "s2",
                        Some(Content::from_text("sub", "user")),
                        RunConfig::default(),
                        RunOptions::default(),
                    );
                    while let Some(item) = s.next().await {
                        item.map_err(|e| ToolError::Execution(Box::new(e)))?;
                    }
                    Ok::<_, ToolError>("ok".to_string())
                }
            },
        )
        .unwrap();
        let parent = llm_agent::new(LlmAgentConfig {
            name: "parent".into(),
            model: Some(ScriptedModel::arc(vec![
                call_content("call_sub", Value::Null),
                model_text("done"),
            ])),
            tools: vec![call_sub],
            ..Default::default()
        })
        .unwrap();
        let r = runner_with(parent, "app", vec![recorder_plugin(&rec)]);
        drain_ok(run(&r, "u", "s", "go")).await;
        let (rec, own) = (rec.lock(), own.lock());
        if own_plugins {
            assert_eq!(
                rec.before_agents,
                ["parent"],
                "parent plugin, own-plugins sub-runner"
            );
            assert_eq!(own.before_agents, ["leaf"], "sub-runner's own plugin");
        } else {
            assert_eq!(
                rec.before_agents,
                ["parent", "leaf"],
                "parent plugin, plugin-less sub-runner"
            );
            assert!(rec.before_models.iter().any(|a| a == "leaf"));
        }
        // Run-level callbacks stay with the runner that owns the run.
        assert_eq!(rec.user_msgs, ["go"], "parent OnUserMessageCallback");
        assert_eq!(rec.before_runs, 1, "parent BeforeRunCallback");
    }
}
