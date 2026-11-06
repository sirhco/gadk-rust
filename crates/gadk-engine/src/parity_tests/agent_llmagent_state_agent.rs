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

//! Port of Go agent/llmagent/state_agent_test.go.

use std::any::Any;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use gadk_agent::{
    AfterAgentCallback, AfterModelCallback, AfterToolCallback, BeforeAgentCallback,
    BeforeModelCallback, BeforeToolCallback, Context, ReadonlyContext, RunConfig, Tool, ToolError,
    Toolset,
};
use gadk_genai::{Content, FunctionCall, ROLE_MODEL, ROLE_USER};
use gadk_model::{Llm, LlmRequest, LlmResponse, ModelError};
use gadk_platform::Platform;
use gadk_session::{CreateRequest, GetRequest, InMemorySessionService, SessionService};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::llm::LlmAgentConfig;
use crate::llm::agent as llm_agent;
use crate::runner::{PluginConfig, RunOptions, Runner, RunnerConfig};
use crate::tool::utils::{append_instructions, text_parts};
use crate::tool::{FunctionTool, FunctionToolConfig};

type GenFn = Arc<dyn Fn(&LlmRequest, bool) -> Result<LlmResponse, ModelError> + Send + Sync>;

/// Port of Go FakeLLM (default reply "fake model response").
#[derive(Default)]
struct FakeLlm {
    f: Option<GenFn>,
}
impl Llm for FakeLlm {
    fn name(&self) -> &str {
        "fake-llm"
    }
    fn generate_content(
        &self,
        req: LlmRequest,
        stream: bool,
    ) -> BoxStream<'static, Result<LlmResponse, ModelError>> {
        let out = match &self.f {
            Some(f) => f(&req, stream),
            None => Ok(LlmResponse {
                content: Some(Content::from_text("fake model response", ROLE_MODEL)),
                ..Default::default()
            }),
        };
        futures::stream::once(async move { out }).boxed()
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn runner_over(agent: Arc<dyn gadk_agent::Agent>, svc: Arc<dyn SessionService>) -> Runner {
    Runner::new(RunnerConfig {
        app_name: "test_app".into(),
        agent,
        session_service: svc,
        artifact_service: None,
        memory_service: None,
        plugin_config: PluginConfig::default(),
        auto_create_session: false,
        platform: Platform::default(),
        compaction: None,
    })
    .unwrap()
}

/// Port of Go assertSessionValues; failures are collected (a panic inside a callback future
/// would surface as a stream error instead of a test failure).
async fn check_session(
    svc: &Arc<dyn SessionService>,
    ctx: &Context,
    title: &str,
    in_ctx: &[&str],
    in_service: &[&str],
    not_in_service: &[&str],
    failures: &Mutex<Vec<String>>,
) {
    let sess = svc
        .get(GetRequest {
            app_name: ctx.app_name().into(),
            user_id: ctx.user_id().into(),
            session_id: ctx.session_id().into(),
            ..Default::default()
        })
        .await
        .unwrap();
    let mut f = failures.lock().unwrap();
    for k in in_ctx {
        if ctx.state().get(k).is_err() {
            f.push(format!("[{title}] key {k} not found in context state"));
        }
    }
    for k in in_service {
        if sess.state().get(k).is_err() {
            f.push(format!(
                "[{title}] key {k} not found in service session state"
            ));
        }
    }
    for k in not_in_service {
        if let Ok(v) = sess.state().get(k) {
            f.push(format!(
                "[{title}] key {k} unexpectedly in service session state: {v}"
            ));
        }
    }
}

#[tokio::test]
async fn test_agent_session_lifecycle() {
    let svc: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    let failures = Arc::new(Mutex::new(Vec::<String>::new()));

    let before_agent: BeforeAgentCallback = {
        let (svc, failures) = (svc.clone(), failures.clone());
        Arc::new(move |ctx: Context| {
            let (svc, failures) = (svc.clone(), failures.clone());
            Box::pin(async move {
                if ctx.state().get("before_agent_callback_state_key").is_ok() {
                    return Ok(Some(Content::from_text(
                        "Sorry, I can only reply once.",
                        ROLE_MODEL,
                    )));
                }
                ctx.state().set(
                    "before_agent_callback_state_key",
                    json!("before_agent_callback_state_value"),
                )?;
                check_session(
                    &svc,
                    &ctx,
                    "In before_agent_callback",
                    &["before_agent_callback_state_key"],
                    &[],
                    &["before_agent_callback_state_key"],
                    &failures,
                )
                .await;
                Ok(None)
            })
        })
    };
    let before_model: BeforeModelCallback = {
        let (svc, failures) = (svc.clone(), failures.clone());
        Arc::new(move |ctx: Context, req: LlmRequest| {
            let (svc, failures) = (svc.clone(), failures.clone());
            Box::pin(async move {
                if let Err(e) = ctx.state().set(
                    "before_model_callback_state_key",
                    json!("before_model_callback_state_value"),
                ) {
                    return (req, Err(e.into()));
                }
                check_session(
                    &svc,
                    &ctx,
                    "In before_model_callback",
                    &[
                        "before_agent_callback_state_key",
                        "before_model_callback_state_key",
                    ],
                    &["before_agent_callback_state_key"],
                    &["before_model_callback_state_key"],
                    &failures,
                )
                .await;
                (req, Ok(None))
            })
        })
    };
    let after_model: AfterModelCallback = {
        let (svc, failures) = (svc.clone(), failures.clone());
        Arc::new(
            move |ctx: Context, _resp: LlmResponse, _err: Option<Arc<ModelError>>| {
                let (svc, failures) = (svc.clone(), failures.clone());
                Box::pin(async move {
                    ctx.state().set(
                        "after_model_callback_state_key",
                        json!("after_model_callback_state_value"),
                    )?;
                    check_session(
                        &svc,
                        &ctx,
                        "In after_model_callback",
                        &[
                            "before_agent_callback_state_key",
                            "before_model_callback_state_key",
                            "after_model_callback_state_key",
                        ],
                        &["before_agent_callback_state_key"],
                        &[
                            "before_model_callback_state_key",
                            "after_model_callback_state_key",
                        ],
                        &failures,
                    )
                    .await;
                    Ok(None)
                })
            },
        )
    };
    let after_agent: AfterAgentCallback = {
        let (svc, failures) = (svc.clone(), failures.clone());
        Arc::new(move |ctx: Context| {
            let (svc, failures) = (svc.clone(), failures.clone());
            Box::pin(async move {
                ctx.state().set(
                    "after_agent_callback_state_key",
                    json!("after_agent_callback_state_value"),
                )?;
                check_session(
                    &svc,
                    &ctx,
                    "In after_agent_callback",
                    &[
                        "before_agent_callback_state_key",
                        "before_model_callback_state_key",
                        "after_model_callback_state_key",
                        "after_agent_callback_state_key",
                    ],
                    &[
                        "before_agent_callback_state_key",
                        "before_model_callback_state_key",
                        "after_model_callback_state_key",
                    ],
                    &["after_agent_callback_state_key"],
                    &failures,
                )
                .await;
                Ok(None)
            })
        })
    };

    let fake: Arc<dyn Llm> = Arc::new(FakeLlm {
        f: Some(Arc::new(|_req: &LlmRequest, _stream: bool| {
            Ok(LlmResponse {
                content: Some(Content::from_text("test model response", ROLE_MODEL)),
                ..Default::default()
            })
        })),
    });
    let root = llm_agent::new(LlmAgentConfig {
        name: "root_agent".into(),
        description: "a verification agent.".into(),
        instruction: "Test instruction".into(),
        model: Some(fake),
        before_agent_callbacks: vec![before_agent],
        before_model_callbacks: vec![before_model],
        after_model_callbacks: vec![after_model],
        after_agent_callbacks: vec![after_agent],
        ..Default::default()
    })
    .unwrap();
    let r = runner_over(root, svc.clone());
    let sess = svc
        .create(CreateRequest {
            app_name: "test_app".into(),
            user_id: "test_user".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    let mut stream = r.run(
        "test_user",
        sess.id(),
        Some(Content::from_text("Hello agent", ROLE_USER)),
        RunConfig::default(),
        RunOptions::default(),
    );
    while let Some(ev) = stream.next().await {
        ev.expect("agent run");
    }
    let fin = svc
        .get(GetRequest {
            app_name: "test_app".into(),
            user_id: "test_user".into(),
            session_id: sess.id().into(),
            ..Default::default()
        })
        .await
        .unwrap();
    for k in [
        "before_agent_callback_state_key",
        "before_model_callback_state_key",
        "after_model_callback_state_key",
        "after_agent_callback_state_key",
    ] {
        assert!(
            fin.state().get(k).is_ok(),
            "key {k} not found in final session state"
        );
    }
    let f = failures.lock().unwrap();
    assert!(f.is_empty(), "{f:#?}");
}

#[derive(Deserialize, JsonSchema)]
struct WeatherArgs {
    location: String,
}
#[derive(Serialize, JsonSchema)]
struct WeatherResult {
    location: String,
    temperature: i64,
    condition: String,
    humidity: i64,
    timestamp: String,
}
#[derive(Deserialize, JsonSchema)]
struct CalculationArgs {
    operation: String,
    x: f64,
    y: f64,
}
#[derive(Serialize, JsonSchema)]
struct CalculationResult {
    operation: String,
    x: f64,
    y: f64,
    result: Value,
    timestamp: String,
}
#[derive(Deserialize, JsonSchema)]
struct LogActivityParams {
    message: String,
}
#[derive(Serialize, JsonSchema)]
struct LogActivityResult {
    status: String,
    entry: Value,
    total_entries: usize,
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Port of Go GetWeather; Go draws the values from math/rand, the port uses fixed values
/// (no test asserts them).
async fn get_weather(_ctx: Context, args: WeatherArgs) -> Result<WeatherResult, ToolError> {
    Ok(WeatherResult {
        location: args.location,
        temperature: 20,
        condition: "sunny".into(),
        humidity: 50,
        timestamp: now(),
    })
}
async fn calculate(_ctx: Context, args: CalculationArgs) -> Result<CalculationResult, ToolError> {
    // Go: "divide" is only offered when the operation is exactly "divide" (the lookup is
    // lower-cased); division by zero gives ±Inf, which JSON (serde_json) renders as null — the
    // validation callback blocks that case before the tool runs.
    let result = match args.operation.to_lowercase().as_str() {
        "add" => json!(args.x + args.y),
        "subtract" => json!(args.x - args.y),
        "multiply" => json!(args.x * args.y),
        "divide" if args.operation == "divide" => json!(args.x / args.y),
        _ => json!("Unknown operation"),
    };
    Ok(CalculationResult {
        operation: args.operation,
        x: args.x,
        y: args.y,
        result,
        timestamp: now(),
    })
}
async fn log_activity(
    ctx: Context,
    params: LogActivityParams,
) -> Result<LogActivityResult, ToolError> {
    let mut log = match ctx.state().get("activity_log") {
        Ok(Value::Array(a)) => a,
        _ => Vec::new(),
    };
    let entry = json!({"timestamp": now(), "message": params.message});
    log.push(entry.clone());
    let total = log.len();
    ctx.state()
        .set("activity_log", Value::Array(log))
        .map_err(|e| ToolError::Execution(Box::new(e)))?;
    Ok(LogActivityResult {
        status: "logged".into(),
        entry,
        total_entries: total,
    })
}

fn tool_cb<F>(f: F) -> BeforeToolCallback
where
    F: Fn(
            &Context,
            &str,
            &Map<String, Value>,
        ) -> Result<Option<Map<String, Value>>, gadk_agent::AgentError>
        + Send
        + Sync
        + 'static,
{
    Arc::new(move |ctx, tool, args| {
        let out = f(&ctx, tool.name(), &args);
        Box::pin(async move { out })
    })
}

// parity: adapted: Go asserts on fmt "%v" of the result map ("enhanced:true"); Rust asserts the same keys/values on the JSON map
#[tokio::test]
async fn test_tool_callbacks_agent() {
    let svc: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    let fake: Arc<dyn Llm> = Arc::new(FakeLlm {
        f: Some(Arc::new(|req: &LlmRequest, _stream: bool| {
            let user_text = if req.contents.len() == 1 && !req.contents[0].parts.is_empty() {
                req.contents[0].parts[0].text.clone()
            } else if req.contents.len() > 1 {
                "after func".to_string()
            } else {
                String::new()
            };
            let (name, args) = match user_text.as_str() {
                "weather in London" => ("get_weather", json!({"location": "London"})),
                "weather in secret" => ("get_weather", json!({"location": "secret"})),
                "calculate 5 plus 3" => {
                    ("calculate", json!({"operation": "add", "x": 5.0, "y": 3.0}))
                }
                "calculate 5 divide by 0" => (
                    "calculate",
                    json!({"operation": "divide", "x": 5.0, "y": 0.0}),
                ),
                "log this message" => ("log_activity", json!({"message": "test log"})),
                "after func" => {
                    return Ok(LlmResponse {
                        content: Some(Content::from_text("Function Ended", ROLE_MODEL)),
                        ..Default::default()
                    });
                }
                _ => {
                    return Ok(LlmResponse {
                        content: Some(Content::from_text(
                            "I'm not sure how to respond to that.",
                            ROLE_MODEL,
                        )),
                        ..Default::default()
                    });
                }
            };
            let Value::Object(args) = args else {
                unreachable!()
            };
            let fc = FunctionCall {
                name: name.into(),
                args,
                ..Default::default()
            };
            Ok(LlmResponse {
                content: Some(Content::from_function_call(fc, ROLE_MODEL)),
                ..Default::default()
            })
        })),
    });

    let get_weather_tool = FunctionTool::new(
        FunctionToolConfig::new("get_weather", "Get weather information"),
        get_weather,
    )
    .unwrap();
    let calculate_tool = FunctionTool::new(
        FunctionToolConfig::new("calculate", "Perform mathematical calculations"),
        calculate,
    )
    .unwrap();
    let log_activity_tool = FunctionTool::new(
        FunctionToolConfig::new("log_activity", "Log an activity message"),
        log_activity,
    )
    .unwrap();

    let audit = tool_cb(|ctx, name, args| {
        let mut log = match ctx.state().get("audit_log") {
            Ok(Value::Array(a)) => a,
            _ => Vec::new(),
        };
        log.push(
            json!({"type": "before_call", "tool_name": name, "args": args, "timestamp": now()}),
        );
        ctx.state().set("audit_log", Value::Array(log))?;
        Ok(None)
    });
    let security = tool_cb(|ctx, name, args| {
        if name == "get_weather" {
            let location = args
                .get("location")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if ["classified", "secret"].contains(&location.to_lowercase().as_str()) {
                ctx.state().set("security_log", json!("example"))?;
                let Value::Object(m) = json!({"error": "Access denied", "reason": "Location access is restricted", "requested_location": location})
                else {
                    unreachable!()
                };
                return Ok(Some(m));
            }
        }
        Ok(None)
    });
    let validation = tool_cb(|_ctx, name, args| {
        if name == "calculate" {
            let op = args
                .get("operation")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if op.to_lowercase() == "divide" && args.get("y").and_then(Value::as_f64) == Some(0.0) {
                let Value::Object(m) = json!({"error": "Division by zero", "operation": op, "x": args.get("x"), "y": args.get("y")})
                else {
                    unreachable!()
                };
                return Ok(Some(m));
            }
        }
        Ok(None)
    });
    let enhance: AfterToolCallback = Arc::new(|_ctx, tool, _args, result, err| {
        let name = tool.name().to_string();
        Box::pin(async move {
            if err.is_some() {
                return Ok(Some(result));
            }
            let mut m = result;
            m.insert("enhanced".into(), json!(true));
            m.insert("enhancement_timestamp".into(), json!(now()));
            m.insert("tool_name".into(), json!(name));
            m.insert("execution_context".into(), json!("live_streaming"));
            Ok(Some(m))
        })
    });
    let async_after: AfterToolCallback = Arc::new(|_ctx, _tool, _args, result, err| {
        Box::pin(async move {
            if err.is_some() {
                return Ok(Some(result));
            }
            let mut m = result;
            m.insert("async_processed".into(), json!(true));
            m.insert("processor".into(), json!("async_after_callback"));
            Ok(Some(m))
        })
    });

    let root = llm_agent::new(LlmAgentConfig {
        name: "tool_callbacks_agent".into(),
        description: "Agent to test tool callbacks".into(),
        model: Some(fake),
        instruction: "Follow user instructions to call tools.".into(),
        tools: vec![get_weather_tool, calculate_tool, log_activity_tool],
        before_tool_callbacks: vec![audit, security, validation],
        after_tool_callbacks: vec![enhance, async_after],
        ..Default::default()
    })
    .unwrap();
    let r = runner_over(root, svc.clone());

    // (query, required (key, value) pairs — Value::Null means "key present", forbidden keys, state keys)
    type Case = (
        &'static str,
        Vec<(&'static str, Value)>,
        Vec<&'static str>,
        Vec<&'static str>,
    );
    let cases: Vec<Case> = vec![
        (
            "weather in London",
            vec![
                ("location", json!("London")),
                ("temperature", Value::Null),
                ("enhanced", json!(true)),
            ],
            vec!["async_processed"],
            vec![],
        ),
        (
            "weather in secret",
            vec![
                ("error", json!("Access denied")),
                ("reason", json!("Location access is restricted")),
                ("enhanced", json!(true)),
            ],
            vec![],
            vec!["security_log"],
        ),
        (
            "calculate 5 plus 3",
            vec![
                ("operation", json!("add")),
                ("result", json!(8.0)),
                ("enhanced", json!(true)),
            ],
            vec![],
            vec![],
        ),
        (
            "calculate 5 divide by 0",
            vec![
                ("error", json!("Division by zero")),
                ("enhanced", json!(true)),
            ],
            vec![],
            vec![],
        ),
        (
            "log this message",
            vec![
                ("status", json!("logged")),
                ("total_entries", json!(1)),
                ("enhanced", json!(true)),
            ],
            vec![],
            vec!["activity_log"],
        ),
    ];
    for (query, want, dont_want, state_keys) in cases {
        let sess = svc
            .create(CreateRequest {
                app_name: "test_app".into(),
                user_id: "test_user".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        let mut stream = r.run(
            "test_user",
            sess.id(),
            Some(Content::from_text(query, ROLE_USER)),
            RunConfig::default(),
            RunOptions::default(),
        );
        let mut results = Vec::new();
        while let Some(ev) = stream.next().await {
            let ev = ev.unwrap();
            for p in ev.response.content.iter().flat_map(|c| c.parts.iter()) {
                if let Some(fr) = &p.function_response {
                    results.push(fr.response.clone());
                }
            }
        }
        let last = results
            .last()
            .unwrap_or_else(|| panic!("[{query}] expected tool results, got none"));
        for (k, v) in &want {
            let got = last
                .get(*k)
                .unwrap_or_else(|| panic!("[{query}] key {k} missing in {last:?}"));
            if !v.is_null() {
                assert_eq!(
                    got.as_f64().map(Value::from).unwrap_or_else(|| got.clone()),
                    v.as_f64().map(Value::from).unwrap_or_else(|| v.clone()),
                    "[{query}] {k}"
                );
            }
        }
        for k in &dont_want {
            assert!(
                !last.contains_key(*k),
                "[{query}] unexpected key {k} in {last:?}"
            );
        }
        if !state_keys.is_empty() {
            let cur = svc
                .get(GetRequest {
                    app_name: "test_app".into(),
                    user_id: "test_user".into(),
                    session_id: sess.id().into(),
                    ..Default::default()
                })
                .await
                .unwrap();
            for k in &state_keys {
                assert!(
                    cur.state().get(k).is_ok(),
                    "[{query}] state key {k} missing"
                );
            }
        }
    }
}

/// Port of Go mockToolset.
struct MockToolset;
#[async_trait]
impl Toolset for MockToolset {
    fn name(&self) -> &str {
        "test_toolset"
    }
    async fn tools(&self, _ctx: &ReadonlyContext) -> Result<Vec<Arc<dyn Tool>>, ToolError> {
        Ok(vec![])
    }
    async fn process_request(&self, _ctx: &Context, req: &mut LlmRequest) -> Result<(), ToolError> {
        append_instructions(req, &["Extra instruction from mockToolset"]);
        Ok(())
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[tokio::test]
async fn test_agent_toolset_preprocess_effect() {
    let captured: Arc<Mutex<Option<LlmRequest>>> = Arc::default();
    let sink = captured.clone();
    let fake_llm = FakeLlm {
        f: Some(Arc::new(move |req: &LlmRequest, _stream: bool| {
            *sink.lock().unwrap() = Some(req.clone());
            Ok(LlmResponse {
                content: Some(Content::from_text("fake response", ROLE_MODEL)),
                ..Default::default()
            })
        })),
    };
    let root_agent = llm_agent::new(LlmAgentConfig {
        name: "toolset_effect_agent".into(),
        instruction: "Agent instruction.".into(),
        model: Some(Arc::new(fake_llm)),
        toolsets: vec![Arc::new(MockToolset)],
        ..Default::default()
    })
    .expect("Failed to create LLM Agent");
    let svc: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    let runner = runner_over(root_agent, svc.clone());
    let sess = svc
        .create(CreateRequest {
            app_name: "test_app".into(),
            user_id: "test_user".into(),
            ..Default::default()
        })
        .await
        .expect("Failed to create session");

    let mut stream = runner.run(
        "test_user",
        sess.id(),
        Some(Content::from_text("Hello", ROLE_USER)),
        RunConfig::default(),
        RunOptions::default(),
    );
    while let Some(item) = stream.next().await {
        item.expect("Error during agent run");
    }

    let req = captured
        .lock()
        .unwrap()
        .clone()
        .expect("LLMRequest was not captured");
    let system_instruction = text_parts(
        req.config
            .as_ref()
            .and_then(|c| c.system_instruction.as_ref()),
    )
    .join(" ");
    assert!(
        system_instruction.contains("Extra instruction from mockToolset"),
        "got SystemInstruction = {system_instruction:?}, want it to contain \"Extra instruction from mockToolset\""
    );
}
