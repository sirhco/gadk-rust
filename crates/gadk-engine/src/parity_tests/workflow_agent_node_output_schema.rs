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

//! Port of Go workflow/agent_node_output_schema_test.go.
//! External package `workflow_test`, mapped to an internal target because it reads
//! `llminternal.Reveal` (Go: be529e7b, #2194 — a typed node passes its output schema to the model).

use std::any::Any;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use futures::StreamExt;
use futures::stream::BoxStream;
use gadk_agent::{Agent, BeforeModelCallback, Context, Mode, RunConfig, Tool, ToolError};
use gadk_genai::{Content, FunctionCall, GenerateContentConfig, Part, Schema, Type};
use gadk_model::{Backend, Llm, LlmRequest, LlmResponse, ModelError};
use gadk_session::{CreateRequest, InMemorySessionService, SessionService};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::llm::state::reveal;
use crate::llm::{LlmAgentConfig, agent as llm_agent};
use crate::tool::utils::infer_schema;
use crate::tool::{FunctionTool, FunctionToolConfig};
use crate::workflow::{AgentNode, Node, NodeConfig};

#[derive(Serialize, Deserialize, schemars::JsonSchema)]
struct WeatherOut {
    city: String,
    temp: i64,
}

fn prop(t: Type) -> Schema {
    Schema {
        r#type: Some(t),
        ..Schema::default()
    }
}

/// Go `weatherSchema`.
fn weather_schema() -> Schema {
    Schema {
        r#type: Some(Type::Object),
        properties: BTreeMap::from([
            ("city".to_string(), prop(Type::String)),
            ("temp".to_string(), prop(Type::Integer)),
        ]),
        required: vec!["city".into(), "temp".into()],
        ..Schema::default()
    }
}

/// Port of Go `requestRecorder`: records every request; on a Gemini API model offered
/// set_model_response, the first call answers with that tool.
#[derive(Default)]
struct RequestRecorder {
    name: String,
    backend: Option<Backend>,
    reqs: Mutex<Vec<LlmRequest>>,
    /// Text answer; defaults to a WeatherOut value.
    reply: String,
    set_reply: Option<Map<String, Value>>,
    /// Makes the set_model_response call carry no args at all.
    set_no_args: bool,
}

impl RequestRecorder {
    fn named(name: &str) -> Arc<Self> {
        Arc::new(Self {
            name: name.into(),
            ..Self::default()
        })
    }
    /// Go `&requestRecorder{name: "recorder"}`.
    fn plain() -> Self {
        Self {
            name: "recorder".into(),
            ..Self::default()
        }
    }
    fn gemini() -> Self {
        Self {
            name: "gemini-2.5-flash".into(),
            backend: Some(Backend::GeminiApi),
            ..Self::default()
        }
    }
    fn reqs(&self) -> Vec<LlmRequest> {
        self.reqs.lock().clone()
    }
    /// Go `firstRequest`.
    fn first_request(&self) -> LlmRequest {
        self.reqs
            .lock()
            .first()
            .cloned()
            .expect("the model was never called")
    }
}

impl Llm for RequestRecorder {
    fn name(&self) -> &str {
        &self.name
    }
    fn generate_content(
        &self,
        req: LlmRequest,
        _stream: bool,
    ) -> BoxStream<'static, Result<LlmResponse, ModelError>> {
        let offers = offers_tool(&req, "set_model_response");
        let n = {
            let mut reqs = self.reqs.lock();
            reqs.push(req);
            reqs.len()
        };
        let text = if self.reply.is_empty() {
            r#"{"city":"Paris","temp":20}"#.to_string()
        } else {
            self.reply.clone()
        };
        let mut part = Part::from_text(text);
        if (self.set_reply.is_some() || self.set_no_args) && n == 1 && offers {
            part = Part {
                function_call: Some(FunctionCall {
                    name: "set_model_response".into(),
                    args: self.set_reply.clone().unwrap_or_default(),
                    ..FunctionCall::default()
                }),
                ..Part::default()
            };
        }
        let resp = LlmResponse {
            content: Some(Content {
                role: "model".into(),
                parts: vec![part],
            }),
            ..Default::default()
        };
        futures::stream::iter([Ok(resp)]).boxed()
    }
    fn google_variant(&self) -> Option<Backend> {
        self.backend
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Go `offersTool`.
fn offers_tool(req: &LlmRequest, name: &str) -> bool {
    tool_decl(req, name).is_some()
}

/// Go `toolDecl`: the function declaration `name` in the request config, as JSON.
fn tool_decl(req: &LlmRequest, name: &str) -> Option<gadk_genai::FunctionDeclaration> {
    req.config
        .as_ref()?
        .tools
        .iter()
        .flat_map(|t| t.function_declarations.iter())
        .find(|d| d.name == name)
        .cloned()
}

#[derive(Deserialize, schemars::JsonSchema)]
struct LookupArgs {
    q: String,
}

/// Go `lookupTool`.
fn lookup_tool() -> Arc<dyn Tool> {
    FunctionTool::new(
        FunctionToolConfig::new("lookup", "looks a city up"),
        |_c: Context, input: LookupArgs| async move {
            Ok::<_, ToolError>(HashMap::from([("q".to_string(), input.q)]))
        },
    )
    .expect("functiontool.New")
}

fn cfg_of(req: &LlmRequest) -> GenerateContentConfig {
    req.config.clone().unwrap_or_default()
}

/// Port of Go `newRunnableNodeContext`: in-memory session, agent, invocation "inv-test",
/// StreamingMode none.
async fn new_runnable_node_context(a: &Arc<dyn Agent>) -> Context {
    let svc: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    let session = svc
        .create(CreateRequest {
            app_name: "app".into(),
            user_id: "u".into(),
            ..Default::default()
        })
        .await
        .expect("create session");
    Context::builder()
        .session_service(svc)
        .session(session)
        .agent(Arc::clone(a))
        .invocation_id("inv-test")
        .run_config(RunConfig::default())
        .build()
}

/// Go `runNode`: runs `node` once and returns the last output it emitted.
async fn run_node(node: &Arc<AgentNode>, a: &Arc<dyn Agent>) -> Option<Value> {
    let mut s = node.run(
        new_runnable_node_context(a).await,
        json!("weather in Paris?"),
    );
    let mut out = None;
    while let Some(item) = s.next().await {
        let ev = item.unwrap_or_else(|e| panic!("Run: {e}"));
        if ev.output.is_some() {
            out = ev.output;
        }
    }
    out
}

fn new_agent(cfg: LlmAgentConfig) -> Arc<dyn Agent> {
    llm_agent::new(cfg).expect("llmagent.New")
}

/// Go `NewAgentNodeTyped[string, T]`.
fn typed_node<T: schemars::JsonSchema>(a: &Arc<dyn Agent>) -> Arc<AgentNode> {
    AgentNode::with_schemas(
        Arc::clone(a),
        Some(infer_schema::<String>()),
        Some(infer_schema::<T>()),
        NodeConfig::default(),
    )
    .expect("NewAgentNodeTyped")
}

type BuildNode = Box<dyn Fn(&Arc<dyn Agent>) -> Arc<AgentNode>>;

/// Go `untypedAndTypedRequests`: one fresh agent built from `cfg` under an untyped node, another
/// under `build`; the first request each sent.
async fn untyped_and_typed_requests(
    cfg: impl Fn() -> LlmAgentConfig,
    new_llm: impl Fn() -> Arc<RequestRecorder>,
    build: &BuildNode,
) -> (LlmRequest, LlmRequest) {
    let mut out = Vec::new();
    for typed in [false, true] {
        let llm = new_llm();
        let a = new_agent(LlmAgentConfig {
            model: Some(Arc::clone(&llm) as Arc<dyn Llm>),
            ..cfg()
        });
        let node = if typed {
            build(&a)
        } else {
            AgentNode::new(Arc::clone(&a), NodeConfig::default()).expect("NewAgentNode")
        };
        run_node(&node, &a).await;
        out.push(llm.first_request());
    }
    let typed = out.pop().expect("typed");
    (out.pop().expect("untyped"), typed)
}

// parity: adapted: Go's generic NewAgentNodeTyped[string, weatherOut] → AgentNode::with_schemas(inferred).
#[ignore = "8a bug: a typed AgentNode does not pass its output schema to the model (Go be529e7b WithBoundOutputSchema needs a context-carried binding in gadk-agent and outputSchemaFor in llm/processors/basic.rs, neither owned by the D2b fix task)"]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_agent_node_typed_object_output_sets_response_schema() {
    let llm = RequestRecorder::named("recorder");
    let a = new_agent(LlmAgentConfig {
        name: "weather".into(),
        model: Some(Arc::clone(&llm) as Arc<dyn Llm>),
        ..Default::default()
    });
    let node = typed_node::<WeatherOut>(&a);
    run_node(&node, &a).await;
    let cfg = cfg_of(&llm.first_request());
    assert_eq!(
        cfg.response_schema,
        Some(weather_schema()),
        "ResponseSchema mismatch"
    );
    assert_eq!(cfg.response_mime_type, "application/json");
}

// A Gemini API model cannot combine tools with a response schema, so the schema goes out as
// set_model_response and the node returns its arguments.
#[ignore = "8a bug: a typed AgentNode does not pass its output schema to the model (Go be529e7b WithBoundOutputSchema needs a context-carried binding in gadk-agent and outputSchemaFor in llm/processors/basic.rs, neither owned by the D2b fix task)"]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_agent_node_typed_object_output_with_tools_uses_set_model_response() {
    let llm = Arc::new(RequestRecorder {
        // Differs from the default text reply, so the output can only come from the
        // set_model_response call.
        set_reply: Some(
            json!({"city": "Paris", "temp": 31.0})
                .as_object()
                .cloned()
                .expect("object"),
        ),
        ..RequestRecorder::gemini()
    });
    let a = new_agent(LlmAgentConfig {
        name: "weather".into(),
        model: Some(Arc::clone(&llm) as Arc<dyn Llm>),
        tools: vec![lookup_tool()],
        ..Default::default()
    });
    let node = typed_node::<WeatherOut>(&a);
    let out = run_node(&node, &a).await;

    let first = llm.first_request();
    let decl = tool_decl(&first, "set_model_response")
        .expect("set_model_response was not offered to the model");
    assert_eq!(
        decl.parameters_json_schema,
        Some(serde_json::to_value(weather_schema()).expect("schema")),
        "set_model_response parameters mismatch"
    );
    assert_eq!(
        cfg_of(&first).response_schema,
        None,
        "ResponseSchema alongside tools, want nil"
    );
    let got = node
        .validate_output(out.clone().unwrap_or(Value::Null))
        .unwrap_or_else(|e| panic!("ValidateOutput({out:?}): {e}"));
    assert_eq!(
        got,
        json!({"city": "Paris", "temp": 31.0}),
        "node output mismatch"
    );
    // The set_model_response result ends the turn: no second model call.
    assert_eq!(llm.reqs().len(), 1, "model calls, want 1");
}

// An OutputSchema the agent declares is what the model sees, whatever the node's Output type.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_agent_node_declared_output_schema_wins() {
    let declared = Schema {
        r#type: Some(Type::Object),
        properties: BTreeMap::from([("answer".to_string(), prop(Type::String))]),
        ..Schema::default()
    };
    let llm = Arc::new(RequestRecorder {
        reply: r#"{"answer":"sunny"}"#.into(),
        ..RequestRecorder::plain()
    });
    let a = new_agent(LlmAgentConfig {
        name: "weather".into(),
        model: Some(Arc::clone(&llm) as Arc<dyn Llm>),
        output_schema: Some(declared.clone()),
        ..Default::default()
    });
    let node = typed_node::<WeatherOut>(&a);
    run_node(&node, &a).await;
    assert_eq!(
        cfg_of(&llm.first_request()).response_schema,
        Some(declared),
        "ResponseSchema mismatch"
    );
}

#[derive(Serialize, Deserialize, schemars::JsonSchema)]
struct CityOnly {
    city: String,
}

// One agent can back several nodes. Each node's schema reaches only its own runs, and the agent's
// declared OutputSchema is never written.
#[ignore = "8a bug: a typed AgentNode does not pass its output schema to the model (Go be529e7b WithBoundOutputSchema needs a context-carried binding in gadk-agent and outputSchemaFor in llm/processors/basic.rs, neither owned by the D2b fix task)"]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_agent_node_one_agent_two_typed_nodes() {
    let llm = RequestRecorder::named("recorder");
    let a = new_agent(LlmAgentConfig {
        name: "weather".into(),
        model: Some(Arc::clone(&llm) as Arc<dyn Llm>),
        ..Default::default()
    });
    let weather_node = typed_node::<WeatherOut>(&a);
    let city_node = typed_node::<CityOnly>(&a);
    let city_schema = Schema {
        r#type: Some(Type::Object),
        properties: BTreeMap::from([("city".to_string(), prop(Type::String))]),
        required: vec!["city".into()],
        ..Schema::default()
    };
    run_node(&weather_node, &a).await;
    run_node(&city_node, &a).await;
    run_node(&weather_node, &a).await;
    let reqs = llm.reqs();
    assert_eq!(reqs.len(), 3, "model requests, want 3");
    for (i, want) in [weather_schema(), city_schema, weather_schema()]
        .into_iter()
        .enumerate()
    {
        assert_eq!(
            cfg_of(&reqs[i]).response_schema,
            Some(want),
            "run {i} ResponseSchema mismatch"
        );
    }
    assert_eq!(
        reveal(a.as_ref()).expect("llm agent").cfg.output_schema,
        None,
        "agent's declared OutputSchema after the runs, want nil"
    );
}

// Chat and task agents talk to the user between replies, so the node's shape is not forced on
// them: neither a response schema nor set_model_response.
#[rstest::rstest]
#[case::task_no_tools(Mode::Task, false)]
#[case::task_gemini_api_with_tools(Mode::Task, true)]
#[case::chat_no_tools(Mode::Chat, false)]
#[case::chat_gemini_api_with_tools(Mode::Chat, true)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_agent_node_chat_and_task_agents_get_no_output_schema(
    #[case] mode: Mode,
    #[case] gemini_with_tools: bool,
) {
    let llm = Arc::new(if gemini_with_tools {
        RequestRecorder::gemini()
    } else {
        RequestRecorder::plain()
    });
    let a = new_agent(LlmAgentConfig {
        name: "weather".into(),
        model: Some(Arc::clone(&llm) as Arc<dyn Llm>),
        mode,
        tools: if gemini_with_tools {
            vec![lookup_tool()]
        } else {
            Vec::new()
        },
        ..Default::default()
    });
    let node = typed_node::<WeatherOut>(&a);
    run_node(&node, &a).await;
    let req = llm.first_request();
    let cfg = cfg_of(&req);
    assert!(
        cfg.response_schema.is_none() && cfg.response_mime_type.is_empty(),
        "{mode:?} agent request has ResponseSchema {:?}, ResponseMIMEType {:?}; want neither",
        cfg.response_schema,
        cfg.response_mime_type
    );
    assert!(
        !offers_tool(&req, "set_model_response"),
        "{mode:?} agent was offered set_model_response"
    );
}

#[derive(Serialize, Deserialize, schemars::JsonSchema)]
struct WithAnyField {
    summary: String,
    details: Value,
}

#[derive(Serialize, Deserialize, schemars::JsonSchema)]
struct WithMapField {
    counts: HashMap<String, i64>,
}

#[derive(Serialize, Deserialize, schemars::JsonSchema)]
struct Empty {}

fn schema_node(a: &Arc<dyn Agent>, output: Value) -> Arc<AgentNode> {
    AgentNode::with_schemas(Arc::clone(a), None, Some(output), NodeConfig::default())
        .expect("NewAgentNodeWithSchemas")
}

/// The node constructors of Go's TestAgentNode_OutputSchemaLeavesRequestUnchanged.
fn leaves_unchanged_node(name: &str) -> BuildNode {
    match name {
        "string" => Box::new(typed_node::<String>),
        "int" => Box::new(typed_node::<i64>),
        "slice_of_objects" => Box::new(typed_node::<Vec<WeatherOut>>),
        "map" => Box::new(typed_node::<HashMap<String, i64>>),
        "empty_struct" => Box::new(typed_node::<Empty>),
        "struct_with_an_any_field" => Box::new(typed_node::<WithAnyField>),
        "struct_with_a_map_field" => Box::new(typed_node::<WithMapField>),
        "explicit_schema_with_an_integer_enum" => Box::new(|a| {
            schema_node(
                a,
                json!({
                    "type": "object",
                    "properties": {"level": {"type": "integer", "enum": [1, 2, 3]}},
                    "additionalProperties": false,
                }),
            )
        }),
        // No additionalProperties: the node accepts extra keys, which set_model_response would
        // reject.
        "explicit_open_object_schema" => Box::new(|a| {
            schema_node(
                a,
                json!({"type": "object", "properties": {"city": {"type": "string"}}}),
            )
        }),
        "pointer_to_struct" => Box::new(typed_node::<Option<WeatherOut>>),
        other => panic!("unknown node {other}"),
    }
}

// Outputs that are not a closed, non-null object with properties, or that genai.Schema cannot
// describe, must construct and send exactly the request an untyped node sends, with and without
// tools.
// parity: adapted: Go `*weatherOut` (pointer to struct) → Option<WeatherOut>; Go `any` field → serde_json::Value.
#[rstest::rstest]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_agent_node_output_schema_leaves_request_unchanged(
    #[values(false, true)] gemini_with_tools: bool,
    #[values(
        "string",
        "int",
        "slice_of_objects",
        "map",
        "empty_struct",
        "struct_with_an_any_field",
        "struct_with_a_map_field",
        "explicit_schema_with_an_integer_enum",
        "explicit_open_object_schema",
        "pointer_to_struct"
    )]
    node_name: &str,
) {
    let build = leaves_unchanged_node(node_name);
    let (untyped, typed) = untyped_and_typed_requests(
        || LlmAgentConfig {
            name: "weather".into(),
            tools: if gemini_with_tools {
                vec![lookup_tool()]
            } else {
                Vec::new()
            },
            ..Default::default()
        },
        || {
            Arc::new(if gemini_with_tools {
                RequestRecorder::gemini()
            } else {
                RequestRecorder::plain()
            })
        },
        &build,
    )
    .await;
    assert_eq!(
        untyped.config, typed.config,
        "{node_name}: request config differs from an untyped node's"
    );
}

/// The agent setups of Go's TestAgentNode_AgentSetupKeepsItsRequest.
fn setup_case(name: &str, sub: &Arc<dyn Agent>) -> (LlmAgentConfig, RequestRecorder) {
    let declared = Schema {
        r#type: Some(Type::Object),
        properties: BTreeMap::from([("answer".to_string(), prop(Type::String))]),
        ..Schema::default()
    };
    let recorder = |reply: &str| RequestRecorder {
        name: "recorder".into(),
        reply: reply.into(),
        ..RequestRecorder::default()
    };
    match name {
        "response_schema" => (
            LlmAgentConfig {
                generate_content_config: Some(GenerateContentConfig {
                    response_schema: Some(declared),
                    ..GenerateContentConfig::default()
                }),
                ..Default::default()
            },
            recorder(r#"{"answer":"sunny"}"#),
        ),
        "response_json_schema" => (
            LlmAgentConfig {
                generate_content_config: Some(GenerateContentConfig {
                    response_json_schema: Some(
                        json!({"type": "object", "properties": {"answer": {"type": "string"}}}),
                    ),
                    ..GenerateContentConfig::default()
                }),
                ..Default::default()
            },
            recorder(r#"{"answer":"sunny"}"#),
        ),
        "response_mime_type" => (
            LlmAgentConfig {
                generate_content_config: Some(GenerateContentConfig {
                    response_mime_type: "text/plain".into(),
                    ..GenerateContentConfig::default()
                }),
                ..Default::default()
            },
            recorder("sunny"),
        ),
        "sub_agent_on_a_gemini_api_model" => (
            LlmAgentConfig {
                sub_agents: vec![Arc::clone(sub)],
                ..Default::default()
            },
            RequestRecorder::gemini(),
        ),
        other => panic!("unknown case {other}"),
    }
}

// An agent whose own setup makes a response schema unsafe to add sends the same request under a
// typed node as under an untyped one.
#[rstest::rstest]
#[case::generate_content_config_response_schema("response_schema")]
#[case::generate_content_config_response_json_schema("response_json_schema")]
#[case::generate_content_config_response_mime_type("response_mime_type")]
#[case::sub_agent_on_a_gemini_api_model("sub_agent_on_a_gemini_api_model")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_agent_node_agent_setup_keeps_its_request(#[case] name: &str) {
    let sub = new_agent(LlmAgentConfig {
        name: "helper".into(),
        model: Some(RequestRecorder::named("recorder") as Arc<dyn Llm>),
        ..Default::default()
    });
    let build: BuildNode = Box::new(typed_node::<WeatherOut>);
    let (untyped, typed) = untyped_and_typed_requests(
        || LlmAgentConfig {
            name: "weather".into(),
            ..setup_case(name, &sub).0
        },
        || Arc::new(setup_case(name, &sub).1),
        &build,
    )
    .await;
    assert_eq!(
        untyped.config, typed.config,
        "{name}: request config differs from an untyped node's"
    );
}

// A model callback that edits the request's schema changes that request only; the node's next run
// is sent the node's schema unchanged.
#[ignore = "8a bug: a typed AgentNode does not pass its output schema to the model (Go be529e7b WithBoundOutputSchema needs a context-carried binding in gadk-agent and outputSchemaFor in llm/processors/basic.rs, neither owned by the D2b fix task)"]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_agent_node_callback_edit_does_not_reach_next_run() {
    let llm = RequestRecorder::named("recorder");
    let edited = Arc::new(Mutex::new(false));
    let cb: BeforeModelCallback = {
        let edited = Arc::clone(&edited);
        Arc::new(move |_ctx: Context, mut req: LlmRequest| {
            let mut done = edited.lock();
            if !*done && let Some(s) = req.config.as_mut().and_then(|c| c.response_schema.as_mut())
            {
                *done = true;
                s.properties.insert("injected".into(), prop(Type::String));
            }
            Box::pin(async move { (req, Ok(None)) })
        })
    };
    let a = new_agent(LlmAgentConfig {
        name: "weather".into(),
        model: Some(Arc::clone(&llm) as Arc<dyn Llm>),
        before_model_callbacks: vec![cb],
        ..Default::default()
    });
    let node = typed_node::<WeatherOut>(&a);
    run_node(&node, &a).await;
    run_node(&node, &a).await;
    let reqs = llm.reqs();
    assert_eq!(reqs.len(), 2, "model calls, want 2");
    assert_eq!(
        cfg_of(&reqs[1]).response_schema,
        Some(weather_schema()),
        "second run's ResponseSchema mismatch"
    );
}

/// Port of Go `statelessLLM`: the same text on every call, nothing recorded.
struct StatelessLlm;

impl Llm for StatelessLlm {
    fn name(&self) -> &str {
        "stateless"
    }
    fn generate_content(
        &self,
        _req: LlmRequest,
        _stream: bool,
    ) -> BoxStream<'static, Result<LlmResponse, ModelError>> {
        futures::stream::iter([Ok(LlmResponse {
            content: Some(Content::from_text(r#"{"city":"Paris","temp":20}"#, "model")),
            ..Default::default()
        })])
        .boxed()
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

// Concurrent runs of one node, as under a fan-out, each get their own schema.
// parity: adapted: a Rust callback owns its LlmRequest, so two runs cannot share one *genai.Schema; the port checks each run's callback saw the node's schema (Go's pointer-identity check has no counterpart).
#[ignore = "8a bug: a typed AgentNode does not pass its output schema to the model (Go be529e7b WithBoundOutputSchema needs a context-carried binding in gadk-agent and outputSchemaFor in llm/processors/basic.rs, neither owned by the D2b fix task)"]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_agent_node_concurrent_runs_do_not_share_schema() {
    let seen: Arc<Mutex<Vec<Option<Schema>>>> = Arc::default();
    let cb: BeforeModelCallback = {
        let seen = Arc::clone(&seen);
        Arc::new(move |_ctx: Context, mut req: LlmRequest| {
            let schema = req.config.as_ref().and_then(|c| c.response_schema.clone());
            seen.lock().push(schema);
            if let Some(s) = req.config.as_mut().and_then(|c| c.response_schema.as_mut()) {
                s.description = "set by callback".into();
            }
            Box::pin(async move { (req, Ok(None)) })
        })
    };
    let a = new_agent(LlmAgentConfig {
        name: "weather".into(),
        model: Some(Arc::new(StatelessLlm) as Arc<dyn Llm>),
        before_model_callbacks: vec![cb],
        ..Default::default()
    });
    let node = typed_node::<WeatherOut>(&a);
    let mut runs = Vec::new();
    for _ in 0..2 {
        let ctx = new_runnable_node_context(&a).await;
        let node = Arc::clone(&node);
        runs.push(tokio::spawn(async move {
            let mut s = node.run(ctx, json!("weather in Paris?"));
            while let Some(item) = s.next().await {
                if let Err(e) = item {
                    panic!("Run: {e}");
                }
            }
        }));
    }
    for r in runs {
        r.await.expect("run task");
    }
    let seen = seen.lock().clone();
    assert!(
        seen.len() == 2 && seen.iter().all(|s| s.as_ref() == Some(&weather_schema())),
        "callback saw schemas {seen:?}, want two equal to the node's"
    );
}

// An agent that declares single_turn itself is treated like one that runs single_turn by default
// at a node.
#[ignore = "8a bug: a typed AgentNode does not pass its output schema to the model (Go be529e7b WithBoundOutputSchema needs a context-carried binding in gadk-agent and outputSchemaFor in llm/processors/basic.rs, neither owned by the D2b fix task)"]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_agent_node_explicit_single_turn_agent_gets_output_schema() {
    let llm = RequestRecorder::named("recorder");
    let a = new_agent(LlmAgentConfig {
        name: "weather".into(),
        model: Some(Arc::clone(&llm) as Arc<dyn Llm>),
        mode: Mode::SingleTurn,
        ..Default::default()
    });
    let node = typed_node::<WeatherOut>(&a);
    run_node(&node, &a).await;
    assert_eq!(
        cfg_of(&llm.first_request()).response_schema,
        Some(weather_schema()),
        "ResponseSchema mismatch"
    );
}

// parity: skipped TestAgentNode_InnerUntypedPlacementClearsOuterSchema: needs llminternal WithBoundOutputSchema (gadk has no bound-output-schema API; fix task, be529e7b)

// A hand-written object schema that allows extra keys is not sent: the model's answer with an
// extra key reaches the node as it does on an untyped node.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_agent_node_open_object_schema_keeps_model_answer() {
    let answer = json!({"city": "Paris", "country": "FR"});
    let llm = Arc::new(RequestRecorder {
        reply: r#"{"city":"Paris","country":"FR"}"#.into(),
        // Used only if set_model_response is offered.
        set_reply: answer.as_object().cloned(),
        ..RequestRecorder::gemini()
    });
    let a = new_agent(LlmAgentConfig {
        name: "weather".into(),
        model: Some(Arc::clone(&llm) as Arc<dyn Llm>),
        tools: vec![lookup_tool()],
        ..Default::default()
    });
    let node = schema_node(
        &a,
        json!({"type": "object", "properties": {"city": {"type": "string"}}}),
    );
    let out = run_node(&node, &a).await;
    let got = node
        .validate_output(out.clone().unwrap_or(Value::Null))
        .unwrap_or_else(|e| panic!("ValidateOutput({out:?}): {e}"));
    assert_eq!(got, answer, "node output mismatch");
}

#[derive(Serialize, Deserialize, schemars::JsonSchema)]
struct CityWithNote {
    city: String,
    note: Option<String>,
}

// A pointer field goes out as nullable, so set_model_response accepts a null for it, and the null
// reaches the output instead of the tool's error.
// parity: adapted: Go `*string` field → Option<String>.
#[ignore = "8a bug: a typed AgentNode does not pass its output schema to the model (Go be529e7b WithBoundOutputSchema needs a context-carried binding in gadk-agent and outputSchemaFor in llm/processors/basic.rs, neither owned by the D2b fix task)"]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_agent_node_null_field_through_set_model_response() {
    let answer = json!({"city": "Paris", "note": null});
    let llm = Arc::new(RequestRecorder {
        // Differs, so a pass needs the tool call.
        reply: r#"{"city":"Lyon","note":"text"}"#.into(),
        set_reply: answer.as_object().cloned(),
        ..RequestRecorder::gemini()
    });
    let a = new_agent(LlmAgentConfig {
        name: "weather".into(),
        model: Some(Arc::clone(&llm) as Arc<dyn Llm>),
        tools: vec![lookup_tool()],
        ..Default::default()
    });
    let node = typed_node::<CityWithNote>(&a);
    let out = run_node(&node, &a).await;
    assert!(
        offers_tool(&llm.first_request(), "set_model_response"),
        "set_model_response was not offered to the model"
    );
    let got = node
        .validate_output(out.clone().unwrap_or(Value::Null))
        .unwrap_or_else(|e| panic!("ValidateOutput({out:?}): {e}"));
    assert_eq!(got, answer, "node output mismatch");
}

// A set_model_response call without args is the empty object, so it passes a schema whose fields
// are all optional.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_agent_node_set_model_response_without_args() {
    let llm = Arc::new(RequestRecorder {
        // Differs, so a pass needs the tool call.
        reply: r#"{"note":"text"}"#.into(),
        set_no_args: true,
        ..RequestRecorder::gemini()
    });
    let a = new_agent(LlmAgentConfig {
        name: "a".into(),
        model: Some(Arc::clone(&llm) as Arc<dyn Llm>),
        tools: vec![lookup_tool()],
        output_schema: Some(Schema {
            r#type: Some(Type::Object),
            properties: BTreeMap::from([("note".to_string(), prop(Type::String))]),
            ..Schema::default()
        }),
        ..Default::default()
    });
    let node = AgentNode::new(Arc::clone(&a), NodeConfig::default()).expect("NewAgentNode");
    let out = run_node(&node, &a).await;
    assert!(
        offers_tool(&llm.first_request(), "set_model_response"),
        "set_model_response was not offered to the model"
    );
    assert_eq!(out, Some(json!({})), "node output mismatch");
}
