// Copyright 2026 Chris Olson
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

//! Golden tests of the plugin's rows (spec R6) against the Go probe's
//! `tests/fixtures/plugin/*.json` (written by `scripts/gen-go-agentanalytics`, rules in its
//! `SCENARIO.md` "7b-3a plugin row goldens"): each scenario runs through a gadk `Runner` with
//! the plugin installed (scripted model, function tools, a custom agent emitting scripted
//! events, a confirmation tool), the plugin is closed, the rows the fake BigQuery Write server
//! received are decoded, converted and normalized as Go's probe does, and compared with Go's
//! rows field by field (numbers by value).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock};

use chrono::{TimeZone, Utc};
use futures::StreamExt;
use gadk_agent::{Agent, AgentConfig, AgentRunFn, InvocationContext, RunConfig, Tool, ToolError};
use gadk_agentanalytics::testutil::arrow_rows::decode_arrow_rows;
use gadk_agentanalytics::testutil::fake_write_server::FakeWriteServer;
use gadk_agentanalytics::{
    BigQueryClient, ClientOptions, Config, Context,
    new_bigquery_agent_analytics_plugin_with_clients, row::to_json_map,
};
use gadk_engine::llm::LlmAgentConfig;
use gadk_engine::runner::PluginConfig;
use gadk_engine::testutil::MockModel;
use gadk_engine::tool::function::{FunctionTool, FunctionToolConfig};
use gadk_engine::{RunOptions, Runner, RunnerConfig, llm_agent};
use gadk_genai::{Content, FunctionCall, FunctionResponse, Part};
use gadk_model::rest::test_util::{FakeResponse, ScriptedClient};
use gadk_model::{LlmResponse, ModelError};
use gadk_session::{Event, EventCompaction, InMemorySessionService, NodeInfo};
use regex::Regex;
use serde_json::{Map, Value, json};

// ---------------------------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------------------------

fn fixture(scenario: &str) -> Value {
    let path: PathBuf = [
        env!("CARGO_MANIFEST_DIR"),
        "tests",
        "fixtures",
        "plugin",
        &format!("{scenario}.json"),
    ]
    .iter()
    .collect();
    let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_str(&text).unwrap()
}

// ---------------------------------------------------------------------------------------------
// The scenario run (SCENARIO.md "How a scenario runs")
// ---------------------------------------------------------------------------------------------

/// Go `newTestPluginWithConfig`'s BigQuery client: every request answers `200 {}` (a job
/// request a done job).
fn bq_client() -> Arc<BigQueryClient> {
    let http = ScriptedClient::new(|r| {
        if r.path.contains("/jobs") {
            FakeResponse::json(
                r#"{"jobReference":{"projectId":"test-project","jobId":"test-job"},"status":{"state":"DONE"}}"#,
            )
        } else {
            FakeResponse::json("{}")
        }
    });
    let options = ClientOptions {
        http_client: Some(http),
        without_authentication: true,
        endpoint: Some("http://bq.test/bigquery/v2/".into()),
        ..ClientOptions::default()
    };
    Arc::new(BigQueryClient::new("test-project", &options).unwrap())
}

/// Runs `turns` through a runner over `root` with the plugin installed, closes the plugin, and
/// returns the rows the fake write server received, converted (SCENARIO.md "File format").
async fn run_scenario(root: Arc<dyn Agent>, allowlist: &[&str], turns: &[String]) -> Vec<Value> {
    let server = FakeWriteServer::new();
    let config = Config {
        enabled: true,
        project_id: "test-project".into(),
        dataset_id: "test_dataset".into(),
        table_name: "test-table".into(),
        custom_metadata_allowlist: allowlist.iter().map(ToString::to_string).collect(),
        ..Config::default()
    };
    let plugin = new_bigquery_agent_analytics_plugin_with_clients(
        &Context::background(),
        config,
        Some(bq_client()),
        Some(server.client().unwrap()),
    )
    .await
    .unwrap();
    let runner = Runner::new(RunnerConfig {
        app_name: "test-app".into(),
        agent: root,
        session_service: Arc::new(InMemorySessionService::new()),
        artifact_service: None,
        memory_service: None,
        plugin_config: PluginConfig {
            plugins: vec![plugin],
            ..PluginConfig::default()
        },
        auto_create_session: true,
        platform: gadk_platform::Platform::default(),
        compaction: None,
    })
    .unwrap();
    for turn in turns {
        let mut events = runner.run(
            "user-1",
            "session-1",
            Some(Content::from_text(turn.as_str(), "user")),
            RunConfig::default(),
            RunOptions::default(),
        );
        // Errors yielded by the run are part of the scenarios.
        while events.next().await.is_some() {}
    }
    // Close flushes the queue.
    runner.close().await.unwrap();
    server
        .append_requests()
        .iter()
        .flat_map(|r| decode_arrow_rows(&r.request).unwrap())
        .map(|row| convert_row(&row))
        .collect()
}

/// A decoded row as the probe writes it: `timestamp` normalized, the JSON columns (and
/// `content_parts[].object_ref.details`) parsed; `part_attributes` stays text.
fn convert_row(row: &gadk_agentanalytics::Row) -> Value {
    let mut m = to_json_map(row);
    if m.contains_key("timestamp") {
        m.insert("timestamp".into(), json!("<timestamp>"));
    }
    for column in ["content", "attributes", "latency_ms"] {
        if let Some(Value::String(text)) = m.get(column) {
            let parsed: Value = serde_json::from_str(text)
                .unwrap_or_else(|e| panic!("{column} {text:?} is not JSON: {e}"));
            m.insert(column.into(), parsed);
        }
    }
    if let Some(Value::Array(parts)) = m.get_mut("content_parts") {
        for part in parts {
            if let Some(Value::Object(object_ref)) = part.get_mut("object_ref")
                && let Some(Value::String(details)) = object_ref.get("details")
            {
                let parsed: Value = serde_json::from_str(details).unwrap();
                object_ref.insert("details".into(), parsed);
            }
        }
    }
    Value::Object(m)
}

// ---------------------------------------------------------------------------------------------
// Normalization (SCENARIO.md "Normalization")
// ---------------------------------------------------------------------------------------------

static ADK_CALL_ID: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^adk-[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$").unwrap()
});

/// One numbering state per scenario file: per kind, the distinct values in order of first
/// appearance.
#[derive(Default)]
struct Normalizer {
    seen: HashMap<&'static str, HashMap<String, usize>>,
}

impl Normalizer {
    fn ordinal(&mut self, kind: &'static str, value: &str) -> String {
        let seen = self.seen.entry(kind).or_default();
        let next = seen.len() + 1;
        let n = *seen.entry(value.to_owned()).or_insert(next);
        format!("{kind}-{n}")
    }

    fn row(&mut self, row: &mut Value) {
        let Value::Object(m) = row else {
            panic!("row is not an object: {row}");
        };
        // Depth-first, keys in byte-wise order (serde_json's Map is sorted).
        for (key, value) in m.iter_mut() {
            match key.as_str() {
                "timestamp" => *value = json!("<timestamp>"),
                "latency_ms" => {
                    if let Value::Object(lat) = value {
                        for v in lat.values_mut() {
                            if v.is_number() {
                                *v = json!("<ms>");
                            }
                        }
                    }
                }
                _ => self.walk(Some(key), value),
            }
        }
    }

    fn walk(&mut self, key: Option<&str>, value: &mut Value) {
        let kind = match key {
            Some("adk_version") => {
                *value = json!("<adk_version>");
                return;
            }
            Some("event_id") => Some("event"),
            Some("source_event_id") => Some("source-event"),
            Some("invocation_id") => Some("invocation"),
            Some("span_id" | "parent_span_id") => Some("span"),
            Some("trace_id") => Some("trace"),
            _ => None,
        };
        match value {
            Value::String(s) if !s.is_empty() && kind.is_some() => {
                *value = Value::String(self.ordinal(kind.unwrap_or_default(), s));
            }
            Value::String(s) if ADK_CALL_ID.is_match(s) => {
                *value = Value::String(self.ordinal("adk-call", s));
            }
            Value::Object(m) => {
                for (k, v) in m.iter_mut() {
                    self.walk(Some(k), v);
                }
            }
            Value::Array(items) => {
                for v in items {
                    self.walk(None, v);
                }
            }
            _ => {}
        }
    }
}

fn normalize(rows: &mut [Value]) {
    let mut n = Normalizer::default();
    for row in rows {
        n.row(row);
    }
}

// ---------------------------------------------------------------------------------------------
// Comparison (numbers by value)
// ---------------------------------------------------------------------------------------------

fn diff(path: &str, got: Option<&Value>, want: Option<&Value>, out: &mut Vec<String>) {
    match (got, want) {
        (Some(Value::Object(g)), Some(Value::Object(w))) => {
            let mut keys: Vec<&String> = g.keys().chain(w.keys()).collect();
            keys.sort();
            keys.dedup();
            for k in keys {
                diff(&format!("{path}.{k}"), g.get(k), w.get(k), out);
            }
        }
        (Some(Value::Array(g)), Some(Value::Array(w))) => {
            for i in 0..g.len().max(w.len()) {
                diff(&format!("{path}[{i}]"), g.get(i), w.get(i), out);
            }
        }
        (Some(Value::Number(g)), Some(Value::Number(w))) if g.as_f64() == w.as_f64() => {}
        (g, w) if g == w => {}
        (g, w) => out.push(format!(
            "{path}: got {}, want {}",
            g.map_or("<absent>".into(), Value::to_string),
            w.map_or("<absent>".into(), Value::to_string)
        )),
    }
}

/// Runs `scenario`'s turns over `root` and compares the normalized rows with the fixture's.
/// Every field of every row matches Go's: no expected difference is listed.
async fn check(scenario: &str, root: Arc<dyn Agent>) {
    let fx = fixture(scenario);
    let turns: Vec<String> = fx["turns"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t.as_str().unwrap().to_owned())
        .collect();
    let allowlist: Vec<&str> = fx["config"]["CustomMetadataAllowlist"]
        .as_array()
        .map(|a| a.iter().map(|v| v.as_str().unwrap()).collect())
        .unwrap_or_default();
    let mut got = run_scenario(root, &allowlist, &turns).await;
    normalize(&mut got);
    let want = fx["rows"].as_array().unwrap().clone();
    let event_types = |rows: &[Value]| -> Vec<String> {
        rows.iter()
            .map(|r| r["event_type"].as_str().unwrap_or("").to_owned())
            .collect()
    };
    assert_eq!(
        event_types(&got),
        event_types(&want),
        "{scenario}: row event types"
    );
    let mut out = Vec::new();
    for (i, (g, w)) in got.iter().zip(&want).enumerate() {
        diff(
            &format!("rows[{i}]({})", w["event_type"]),
            Some(g),
            Some(w),
            &mut out,
        );
    }
    assert!(
        out.is_empty(),
        "{scenario}: {} differences:\n{}",
        out.len(),
        out.join("\n")
    );
}

// ---------------------------------------------------------------------------------------------
// Scenario agents (SCENARIO.md "Scenarios")
// ---------------------------------------------------------------------------------------------

fn text(t: &str) -> Result<LlmResponse, ModelError> {
    Ok(LlmResponse {
        content: Some(Content::from_text(t, "model")),
        ..Default::default()
    })
}

fn call(id: &str, name: &str, args: Value) -> Result<LlmResponse, ModelError> {
    Ok(LlmResponse {
        content: Some(Content {
            role: "model".into(),
            parts: vec![Part {
                function_call: Some(FunctionCall {
                    id: id.into(),
                    name: name.into(),
                    args: args.as_object().cloned().unwrap_or_default(),
                    ..Default::default()
                }),
                ..Default::default()
            }],
        }),
        ..Default::default()
    })
}

/// A function tool with Go's inferred argument schema (`jsonschema-go` of the Go struct).
fn function_tool<F, Fut>(
    name: &str,
    description: &str,
    schema: Value,
    require_confirmation: bool,
    f: F,
) -> Arc<dyn Tool>
where
    F: Fn(gadk_agent::Context, Map<String, Value>) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Map<String, Value>, ToolError>> + Send + 'static,
{
    FunctionTool::new(
        FunctionToolConfig {
            input_schema: Some(schema),
            require_confirmation,
            ..FunctionToolConfig::new(name, description)
        },
        f,
    )
    .unwrap()
}

fn object(v: Value) -> Map<String, Value> {
    v.as_object().cloned().unwrap()
}

fn llm(cfg: LlmAgentConfig) -> Arc<dyn Agent> {
    llm_agent::new(cfg).unwrap()
}

#[tokio::test]
async fn golden_tool_call() {
    let get_weather = function_tool(
        "get_weather",
        "Returns the weather forecast for a city.",
        json!({"type": "object", "properties": {"city": {"type": "string"}}, "required": ["city"], "additionalProperties": false}),
        false,
        |ctx, args| async move {
            let city = args.get("city").cloned().unwrap_or(Value::Null);
            ctx.update_actions(|a| {
                a.state_delta
                    .get_or_insert_with(Map::new)
                    .insert("last_city".into(), city);
            });
            Ok(object(json!({"forecast": "sunny"})))
        },
    );
    let root = llm(LlmAgentConfig {
        name: "weather_agent".into(),
        description: "Answers weather questions.".into(),
        instruction: "Answer weather questions with the get_weather tool.".into(),
        model: Some(MockModel::new(vec![
            call("call-weather-1", "get_weather", json!({"city": "Paris"})),
            text("It is sunny in Paris."),
        ])),
        tools: vec![get_weather],
        ..Default::default()
    });
    check("tool_call", root).await;
}

#[tokio::test]
async fn golden_tool_error() {
    let divide = function_tool(
        "divide",
        "Divides a by b.",
        json!({"type": "object", "properties": {"a": {"type": "integer"}, "b": {"type": "integer"}}, "required": ["a", "b"], "additionalProperties": false}),
        false,
        |_ctx, args| async move {
            let a = args.get("a").and_then(Value::as_i64).unwrap_or(0);
            let b = args.get("b").and_then(Value::as_i64).unwrap_or(0);
            if b == 0 {
                return Err(ToolError::Execution("division by zero".into()));
            }
            Ok(object(json!({"quotient": a / b})))
        },
    );
    let root = llm(LlmAgentConfig {
        name: "calc_agent".into(),
        model: Some(MockModel::new(vec![
            call("call-divide-1", "divide", json!({"a": 1, "b": 0})),
            text("Cannot divide by zero."),
        ])),
        tools: vec![divide],
        ..Default::default()
    });
    check("tool_error", root).await;
}

#[tokio::test]
async fn golden_transfer() {
    let helper = llm(LlmAgentConfig {
        name: "helper".into(),
        description: "Helps with anything.".into(),
        model: Some(MockModel::new(vec![text("Helper here, how can I help?")])),
        ..Default::default()
    });
    let root = llm(LlmAgentConfig {
        name: "coordinator".into(),
        description: "Routes requests.".into(),
        model: Some(MockModel::new(vec![call(
            "call-transfer-1",
            "transfer_to_agent",
            json!({"agent_name": "helper"}),
        )])),
        sub_agents: vec![helper],
        ..Default::default()
    });
    check("transfer", root).await;
}

#[tokio::test]
async fn golden_model_error() {
    let root = llm(LlmAgentConfig {
        name: "failing_agent".into(),
        model: Some(MockModel::new(vec![Err(ModelError::Other(
            "model failure 404".into(),
        ))])),
        ..Default::default()
    });
    check("model_error", root).await;
}

#[tokio::test]
async fn golden_confirmation() {
    let transfer_funds = function_tool(
        "transfer_funds",
        "Transfers funds.",
        json!({"type": "object", "properties": {"amount": {"type": "integer"}}, "required": ["amount"], "additionalProperties": false}),
        true,
        |_ctx, _args| async move { Ok(object(json!({"status": "done"}))) },
    );
    let root = llm(LlmAgentConfig {
        name: "bank_agent".into(),
        model: Some(MockModel::new(vec![call(
            "call-funds-1",
            "transfer_funds",
            json!({"amount": 100}),
        )])),
        tools: vec![transfer_funds],
        ..Default::default()
    });
    check("confirmation", root).await;
}

/// The eight events of the `session_signals` custom agent.
fn signal_events(ctx: &InvocationContext) -> Vec<Event> {
    let new_event = || {
        let mut ev = Event::new(ctx.platform(), ctx.invocation_id());
        ev.author = "signal_agent".into();
        ev.branch = ctx.branch();
        ev
    };
    let fc = |id: &str, name: &str, args: Value| Part {
        function_call: Some(FunctionCall {
            id: id.into(),
            name: name.into(),
            args: object(args),
            ..Default::default()
        }),
        ..Default::default()
    };
    let fr = |id: &str, name: &str, response: Value| Part {
        function_response: Some(FunctionResponse {
            id: id.into(),
            name: name.into(),
            response: object(response),
            ..Default::default()
        }),
        ..Default::default()
    };

    let mut e1 = new_event();
    e1.actions.compaction = Some(EventCompaction {
        start_timestamp: Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 5).unwrap(),
        end_timestamp: Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 6).unwrap(),
        compacted_content: Some(Content::from_text("Summary of earlier turns.", "model")),
        excluded_events: Vec::new(),
    });

    let mut e2 = new_event();
    e2.response.custom_metadata =
        object(json!({"adk_agent_state": {"step": 1}, "end_of_agent": false}));
    e2.actions.state_delta = Some(object(json!({"progress": "half", "temp:scratch": "x"})));

    let mut e3 = new_event();
    e3.response.content = Some(Content {
        role: "model".into(),
        parts: vec![fc(
            "cred-1",
            "adk_request_credential",
            json!({"auth_scheme": "oauth2"}),
        )],
    });
    e3.long_running_tool_ids = vec!["cred-1".into()];

    let mut e4 = new_event();
    e4.response.content = Some(Content {
        role: "model".into(),
        parts: vec![
            fc(
                "confirm-1",
                "adk_request_confirmation",
                json!({"hint": "Approve?"}),
            ),
            fc(
                "input-1",
                "adk_request_input",
                json!({"message": "Your name?"}),
            ),
        ],
    });

    let mut e5 = new_event();
    e5.response.content = Some(Content {
        role: "user".into(),
        parts: vec![
            fr(
                "cred-1",
                "adk_request_credential",
                json!({"token": "granted"}),
            ),
            fr(
                "confirm-1",
                "adk_request_confirmation",
                json!({"confirmed": true}),
            ),
            fr("input-1", "adk_request_input", json!({"name": "Ada"})),
        ],
    });

    let mut e6 = new_event();
    e6.response.custom_metadata = object(json!({
        "a2a:request": {"method": "message/send"},
        "a2a:response": {"status": "completed"},
    }));

    let mut e7 = new_event();
    e7.node_info = Some(NodeInfo {
        path: "signal_agent/step@1".into(),
        ..Default::default()
    });
    e7.response.error_code = "STEP_FAILED".into();
    e7.response.error_message = "step failed".into();
    e7.output = Some(json!({"result": 42}));

    let mut e8 = new_event();
    e8.response.content = Some(Content::from_text("All signals sent.", "model"));

    vec![e1, e2, e3, e4, e5, e6, e7, e8]
}

#[tokio::test]
async fn golden_session_signals() {
    let run: AgentRunFn = Arc::new(|ctx: InvocationContext| {
        futures::stream::iter(signal_events(&ctx).into_iter().map(Ok)).boxed()
    });
    let root = gadk_agent::new_agent(AgentConfig {
        name: "signal_agent".into(),
        description: "Emits session signals.".into(),
        run: Some(run),
        ..Default::default()
    })
    .unwrap();
    check("session_signals", root).await;
}
