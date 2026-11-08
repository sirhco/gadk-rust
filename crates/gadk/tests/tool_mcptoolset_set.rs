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

#![cfg(all(feature = "gemini", feature = "mcp"))]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Port of Go tool/mcptoolset/set_test.go. The toolsets run over in-process rmcp servers
//! (`gadk::mcp::testutil`); `TestMCPToolSet` and the `TestMCPToolSetConfirmation` subtests drive
//! an LLM agent through the runner, replaying the Gemini traffic from the Go recordings.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;
use gadk::agent::tool::{ConfirmationProvider, Tool, Toolset};
use gadk::agent::{
    Context, ContextDelta, EventStream, REQUEST_CONFIRMATION_FUNCTION_CALL_NAME, ToolError,
    string_predicate,
};
use gadk::engine::llm::{LlmAgentConfig, agent as llm_agent};
use gadk::engine::testutil::{TestAgentRunner, gemini_test_model};
use gadk::genai::{Content, FunctionCall, FunctionResponse, Part, ROLE_MODEL, ROLE_USER};
use gadk::mcp::rmcp::model::{
    CallToolRequestParams, CallToolResult, ContentBlock, JsonObject, Resource, ResourceContents,
    Tool as McpToolDef,
};
use gadk::mcp::rmcp::service::RequestContext;
use gadk::mcp::rmcp::{ErrorData, RoleServer};
use gadk::mcp::testutil::{ScriptedFactory, ToolServer, in_memory, weather_server};
use gadk::mcp::{BoxError, Config, Error, McpToolset, McpTransport, MetadataProvider};
use gadk::model::LlmResponse;
use gadk::session::{Event, NodeInfo, ToolConfirmation};
use pretty_assertions::assert_eq;
use serde_json::{Map, Value, json};

/// Go `modelName`.
const MODEL_NAME: &str = "gemini-2.5-flash";
const TOOL_NAME: &str = "get_weather";
const TOOL_DESCRIPTION: &str = "returns weather in the given city";
const AGENT_NAME: &str = "weather_time_agent";
const CONFIRM_HINT: &str = "Please approve or reject the tool call get_weather() by responding with a FunctionResponse with an expected ToolConfirmation payload.";

/// Go `newGeminiModel`: the recording is `testdata/<t.Name()>.httprr` with `/` (and, for the
/// subtests, the spaces Go's `t.Run` turns into `_`) replaced by `_`.
fn fixture(test_name: &str) -> String {
    format!(
        "{}/tests/testdata/tool/mcptoolset/testdata/{}.httprr",
        env!("CARGO_MANIFEST_DIR"),
        test_name.replace(['/', ' '], "_")
    )
}

fn obj(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        other => panic!("not an object: {other}"),
    }
}

/// Go `mcptoolset.New(mcptoolset.Config{Transport: …})` over `factory`.
fn new_toolset(factory: Arc<dyn gadk::mcp::TransportFactory>) -> McpToolset {
    gadk::mcp::new(Config::new(McpTransport::Custom(factory)))
        .unwrap_or_else(|e| panic!("Failed to create MCP tool set: {e}"))
}

/// Go `icontext.NewInvocationContext(t.Context(), icontext.InvocationContextParams{})`.
fn inv_ctx() -> Context {
    Context::builder().build()
}

/// Go `agent.NewToolContext(invCtx, "", nil, confirmation)`: an empty call id becomes a new UUID.
fn tool_ctx(inv: &Context, confirmation: Option<ToolConfirmation>) -> Context {
    inv.with_delta(ContextDelta {
        function_call_id: Some(String::new()),
        tool_confirmation: Some(confirmation),
        ..Default::default()
    })
}

/// Go `tools[0].(toolinternal.FunctionTool).Run(toolCtx, args)`.
async fn run_tool(
    tool: &Arc<dyn Tool>,
    ctx: Context,
    args: Value,
) -> Result<Map<String, Value>, ToolError> {
    tool.as_runnable()
        .expect("tool is not runnable")
        .run(ctx, args)
        .await
}

/// The crate error behind a `ToolError::Execution`.
fn mcp_error(err: &ToolError) -> &Error {
    match err {
        ToolError::Execution(source) => source
            .downcast_ref::<Error>()
            .unwrap_or_else(|| panic!("not a gadk_mcp::Error: {source}")),
        other => panic!("want ToolError::Execution, got {other:?}"),
    }
}

/// The weather agent of `TestMCPToolSet`/`TestMCPToolSetConfirmation`, replaying `test_name`.
async fn weather_agent_runner(ts: McpToolset, test_name: &str) -> TestAgentRunner {
    let agent = llm_agent::new(LlmAgentConfig {
        name: AGENT_NAME.into(),
        model: Some(gemini_test_model(MODEL_NAME, fixture(test_name)).await),
        description: "Agent to answer questions about the time and weather in a city.".into(),
        instruction: "I can answer your questions about the time and weather in a city.".into(),
        toolsets: vec![Arc::new(ts)],
        ..Default::default()
    })
    .unwrap_or_else(|e| panic!("Failed to create agent: {e}"));
    TestAgentRunner::new(agent).await
}

/// Every event of `s`; an error fails the test (Go `t.Fatal`).
async fn collect(mut s: EventStream) -> Vec<Event> {
    let mut events = Vec::new();
    while let Some(item) = s.next().await {
        match item {
            Ok(e) => events.push(e),
            Err(e) => panic!("runner returned unexpected error: {e}"),
        }
    }
    events
}

/// Go `cmpopts.IgnoreFields(genai.FunctionCall{}, "ID")`, `(genai.FunctionResponse{}, "ID")` and
/// `(genai.Part{}, "ThoughtSignature")`; the first also reaches the `*genai.FunctionCall` held in
/// a confirmation call's `originalFunctionCall` argument.
fn strip_content(c: &mut Content) {
    for p in &mut c.parts {
        p.thought_signature = Default::default();
        if let Some(fc) = p.function_call.as_mut() {
            fc.id.clear();
            if fc.name == REQUEST_CONFIRMATION_FUNCTION_CALL_NAME
                && let Some(Value::Object(orig)) = fc.args.get_mut("originalFunctionCall")
            {
                orig.remove("id");
            }
        }
        if let Some(fr) = p.function_response.as_mut() {
            fr.id.clear();
        }
    }
}

/// Go's `cmpopts.IgnoreFields` of `TestMCPToolSet`: `Event.{ID, Timestamp, InvocationID}`,
/// `EventActions.{StateDelta, ArtifactDelta}`, `LLMResponse.{UsageMetadata, AvgLogprobs,
/// FinishReason}` and the content's ids and thought signatures.
fn normalize(mut ev: Event) -> Event {
    ev.id.clear();
    ev.invocation_id.clear();
    ev.timestamp = gadk::session::zero_time();
    ev.actions.state_delta = None;
    ev.actions.artifact_delta = None;
    ev.response.usage_metadata = None;
    ev.response.avg_logprobs = 0.0;
    ev.response.finish_reason = None;
    if let Some(c) = ev.response.content.as_mut() {
        strip_content(c);
    }
    ev
}

/// A `weather_time_agent` event at node path `weather_time_agent`.
fn agent_event(content: Content, model_version: &str) -> Event {
    Event {
        author: AGENT_NAME.into(),
        node_info: Some(NodeInfo {
            path: AGENT_NAME.into(),
            ..Default::default()
        }),
        response: LlmResponse {
            content: Some(content),
            model_version: model_version.into(),
            ..Default::default()
        },
        ..Default::default()
    }
}

fn fc_content(name: &str, args: Value) -> Content {
    Content::from_function_call(
        FunctionCall {
            name: name.into(),
            args: obj(args),
            ..Default::default()
        },
        ROLE_MODEL,
    )
}

fn fr_content(name: &str, response: Value) -> Content {
    Content::from_function_response(
        FunctionResponse {
            name: name.into(),
            response: obj(response),
            ..Default::default()
        },
        ROLE_USER,
    )
}

#[tokio::test]
async fn test_mcp_tool_set() {
    let ts = new_toolset(in_memory(weather_server));
    let runner = weather_agent_runner(ts, "TestMCPToolSet").await;

    let got: Vec<Event> = collect(runner.run("what is the weather in london?"))
        .await
        .into_iter()
        .map(normalize)
        .collect();

    let want = vec![
        agent_event(
            fc_content(TOOL_NAME, json!({"city": "london"})),
            "gemini-2.5-flash",
        ),
        agent_event(
            fr_content(
                TOOL_NAME,
                json!({"output": {"weather_summary": "Today in \"london\" is sunny"}}),
            ),
            "",
        ),
        agent_event(
            Content::from_text("The weather in London is sunny.", ROLE_MODEL),
            "gemini-2.5-flash",
        ),
    ];
    assert_eq!(got, want, "event[i] mismatch");
}

#[tokio::test]
async fn test_tool_filter() {
    let server = || {
        ToolServer::new("weather_server", "v1.0.0")
            .weather("get_weather", TOOL_DESCRIPTION)
            .weather("get_weather1", TOOL_DESCRIPTION)
    };
    let mut cfg = Config::new(McpTransport::Custom(in_memory(server)));
    cfg.tool_filter = Some(string_predicate(&["get_weather"]));
    let ts = gadk::mcp::new(cfg).unwrap_or_else(|e| panic!("Failed to create MCP tool set: {e}"));

    let tools = ts
        .tools(&inv_ctx())
        .await
        .unwrap_or_else(|e| panic!("Failed to get tools: {e}"));

    let got_tool_names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
    assert_eq!(got_tool_names, vec!["get_weather"], "tools mismatch");
}

/// Waits up to five seconds for `factory`'s server sessions to end (Go `session.Wait()` with a
/// five-second `time.After`).
async fn wait_sessions_closed(factory: &ScriptedFactory) -> bool {
    tokio::time::timeout(Duration::from_secs(5), async {
        while factory.open_sessions() > 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .is_ok()
}

// parity: adapted: Go's io.Closer assertion is the inherent McpToolset::close; the subtests run as a loop
#[tokio::test]
async fn test_mcp_tool_set_close() {
    for connected in [false, true] {
        let name = format!("connected={connected}");
        // Go spyTransport{reconnectableTransport{server}}: a fresh server session per connect.
        let factory =
            ScriptedFactory::new(|| ToolServer::new("test_server", "1").weather("get_weather", ""));
        let ts = new_toolset(factory.clone());
        let inv = inv_ctx();
        let mut tools = Vec::new();
        if connected {
            tools = ts.tools(&inv).await.unwrap();
            assert_eq!(factory.open_sessions(), 1, "{name}: open server sessions");
        }

        let (a, b) = tokio::join!(ts.close(), ts.close());
        for r in [a, b] {
            if let Err(e) = r {
                panic!("{name}: Close() failed: {e}");
            }
        }

        if connected {
            assert!(
                wait_sessions_closed(&factory).await,
                "{name}: Close() left the MCP session open"
            );
            let err = run_tool(&tools[0], tool_ctx(&inv, None), json!({"city": "Paris"}))
                .await
                .unwrap_err();
            assert!(
                mcp_error(&err).is_connection_closed(),
                "{name}: Run() after Close() error = {err}, want ErrConnectionClosed"
            );
        }
        match ts.tools(&inv).await {
            Ok(_) => panic!("{name}: Tools() after Close() succeeded, want ErrConnectionClosed"),
            Err(err) => assert!(
                mcp_error(&err).is_connection_closed(),
                "{name}: Tools() after Close() error = {err}, want ErrConnectionClosed"
            ),
        }
        let want_connections = usize::from(connected);
        assert_eq!(
            factory.connects(),
            want_connections,
            "{name}: Connect() called {} times, want {want_connections}",
            factory.connects()
        );
    }
}

#[tokio::test]
async fn test_list_tools_reconnection() {
    let factory = ScriptedFactory::new(|| {
        ToolServer::new("test_server", "v1.0.0").weather("get_weather", TOOL_DESCRIPTION)
    });
    let ts = new_toolset(factory.clone());
    let ctx = inv_ctx();

    // First call to Tools should create a session.
    ts.tools(&ctx)
        .await
        .unwrap_or_else(|e| panic!("First Tools call failed: {e}"));

    // Kill the transport by closing the connection.
    factory.kill_last().await;

    // Second call should detect the closed connection and reconnect.
    ts.tools(&ctx)
        .await
        .unwrap_or_else(|e| panic!("Second Tools call failed: {e}"));

    // Verify that we reconnected (should have 2 connections).
    assert_eq!(
        factory.connects(),
        2,
        "Expected 2 Connect calls (reconnect after close)"
    );
}

#[tokio::test]
async fn test_call_tool_reconnection() {
    let factory = ScriptedFactory::new(|| {
        ToolServer::new("test_server", "v1.0.0").weather("get_weather", TOOL_DESCRIPTION)
    });
    let ts = new_toolset(factory.clone());
    let inv = inv_ctx();

    // Get tools first to establish a session.
    let tools = ts
        .tools(&inv)
        .await
        .unwrap_or_else(|e| panic!("Tools call failed: {e}"));

    // Kill the transport by closing the connection.
    factory.kill_last().await;

    // Call the tool - should reconnect and succeed.
    let result = run_tool(&tools[0], tool_ctx(&inv, None), json!({"city": "Paris"}))
        .await
        .unwrap_or_else(|e| panic!("Tool call after reconnect failed: {e}"));
    assert!(
        !result.is_empty(),
        "Expected non-nil result after reconnect"
    );

    // Verify that we reconnected (should have 2 connections).
    assert_eq!(
        factory.connects(),
        2,
        "Expected 2 Connect calls (reconnect after close)"
    );
}

/// Go `requireConfirmationProvider`: confirmation for `get_weather` in Lisbon only. Go's
/// `args.(Input)` branch never matches (the flow passes the decoded JSON object), so only the
/// map branch is ported.
fn lisbon_provider() -> ConfirmationProvider {
    Arc::new(|name: &str, args: &Value| {
        if name != TOOL_NAME {
            return false;
        }
        if let Some(m) = args.as_object()
            && let Some(city) = m.get("city")
            && let Some(city) = city.as_str()
        {
            return city == "Lisbon";
        }
        true
    })
}

fn requires_confirmation() -> Content {
    fr_content(
        TOOL_NAME,
        json!({"error": "error tool \"get_weather\" requires confirmation, please approve or reject"}),
    )
}

fn confirmation_call() -> Content {
    fc_content(
        REQUEST_CONFIRMATION_FUNCTION_CALL_NAME,
        json!({
            "originalFunctionCall": serde_json::to_value(FunctionCall {
                name: TOOL_NAME.into(),
                args: obj(json!({"city": "Lisbon"})),
                ..Default::default()
            })
            .unwrap(),
            "toolConfirmation": serde_json::to_value(ToolConfirmation {
                hint: CONFIRM_HINT.into(),
                ..Default::default()
            })
            .unwrap(),
        }),
    )
}

fn sunny(city: &str) -> Content {
    fr_content(
        TOOL_NAME,
        json!({"output": {"weather_summary": format!("Today in \"{city}\" is sunny")}}),
    )
}

fn rejected() -> Content {
    fr_content(
        TOOL_NAME,
        json!({"error": "error tool \"get_weather\" call is rejected"}),
    )
}

fn model_text(text: &str) -> Content {
    Content::from_text(text, ROLE_MODEL)
}

/// One `TestMCPToolSetConfirmation` case.
struct ConfirmationCase {
    name: &'static str,
    require_confirmation: bool,
    provider: Option<ConfirmationProvider>,
    city: &'static str,
    /// The user's confirmation response (`{"confirmed": …}`).
    confirm: Option<bool>,
    want: Vec<Content>,
}

/// The events of `s`, checked one by one against `want` from `*count` on (Go compares each
/// event's content as it arrives); returns the id of the last confirmation call seen.
async fn check_stream(
    name: &str,
    s: EventStream,
    want: &[Content],
    count: &mut usize,
    confirm_call_id: &mut Option<String>,
) {
    for got in collect(s).await {
        assert!(
            *count < want.len(),
            "{name}: stream generated more values than the expected {}. Got: {:?}",
            want.len(),
            got.response.content
        );
        let mut content = got
            .response
            .content
            .clone()
            .unwrap_or_else(|| panic!("{name}: event {count} has no content"));
        for p in &content.parts {
            if let Some(fc) = &p.function_call
                && fc.name == REQUEST_CONFIRMATION_FUNCTION_CALL_NAME
            {
                *confirm_call_id = Some(fc.id.clone());
            }
        }
        strip_content(&mut content);
        assert_eq!(content, want[*count], "{name}: event {count} mismatch");
        *count += 1;
    }
}

// parity: adapted: Go's subtests run as a loop, each replaying TestMCPToolSetConfirmation_<name>.httprr; Go's provider branch for args.(Input) never matches (args are the decoded JSON object) and is not ported
#[tokio::test]
async fn test_mcp_tool_set_confirmation() {
    let static_require = |name, confirm, want| ConfirmationCase {
        name,
        require_confirmation: true,
        provider: None,
        city: "Lisbon",
        confirm,
        want,
    };
    let with_provider = |name, provider, city, confirm, want| ConfirmationCase {
        name,
        require_confirmation: false,
        provider: Some(provider),
        city,
        confirm,
        want,
    };
    let fc = |city: &str| fc_content(TOOL_NAME, json!({"city": city}));
    let sunny_text = |city: &str| model_text(&format!("Today in \"{city}\" is sunny"));
    let sorry = || model_text("I am sorry, I cannot get the weather in Lisbon.");
    let cases = vec![
        ConfirmationCase {
            name: "No Confirmation Required",
            require_confirmation: false,
            provider: None,
            city: "Lisbon",
            confirm: None,
            want: vec![fc("Lisbon"), sunny("Lisbon"), sunny_text("Lisbon")],
        },
        static_require(
            "Confirmation Required",
            None,
            vec![fc("Lisbon"), requires_confirmation(), confirmation_call()],
        ),
        static_require(
            "Confirmation Required and is confirmed",
            Some(true),
            vec![
                fc("Lisbon"),
                requires_confirmation(),
                confirmation_call(),
                sunny("Lisbon"),
                sunny_text("Lisbon"),
            ],
        ),
        static_require(
            "Confirmation Required and is rejected",
            Some(false),
            vec![
                fc("Lisbon"),
                requires_confirmation(),
                confirmation_call(),
                rejected(),
                sorry(),
            ],
        ),
        with_provider(
            "Conditional Confirmation Not Required",
            lisbon_provider(),
            "Porto",
            None,
            vec![fc("Porto"), sunny("Porto"), sunny_text("Porto")],
        ),
        with_provider(
            "Conditional Confirmation Not Required For This Tool",
            Arc::new(|name: &str, _: &Value| name != TOOL_NAME),
            "Lisbon",
            None,
            vec![fc("Lisbon"), sunny("Lisbon"), sunny_text("Lisbon")],
        ),
        with_provider(
            "Conditional Confirmation Required For This Tool",
            Arc::new(|name: &str, _: &Value| name == TOOL_NAME),
            "Lisbon",
            None,
            vec![fc("Lisbon"), requires_confirmation(), confirmation_call()],
        ),
        with_provider(
            "Conditional Confirmation Required",
            lisbon_provider(),
            "Lisbon",
            None,
            vec![fc("Lisbon"), requires_confirmation(), confirmation_call()],
        ),
        with_provider(
            "Conditional Confirmation Required and is confirmed",
            lisbon_provider(),
            "Lisbon",
            Some(true),
            vec![
                fc("Lisbon"),
                requires_confirmation(),
                confirmation_call(),
                sunny("Lisbon"),
                sunny_text("Lisbon"),
            ],
        ),
        with_provider(
            "Conditional Confirmation Required and is rejected",
            lisbon_provider(),
            "Lisbon",
            Some(false),
            vec![
                fc("Lisbon"),
                requires_confirmation(),
                confirmation_call(),
                rejected(),
                sorry(),
            ],
        ),
    ];
    assert_eq!(cases.len(), 10);

    for tc in cases {
        let name = tc.name;
        let mut cfg = Config::new(McpTransport::Custom(in_memory(weather_server)));
        cfg.require_confirmation = tc.require_confirmation;
        cfg.require_confirmation_provider = tc.provider;
        let ts = gadk::mcp::new(cfg)
            .unwrap_or_else(|e| panic!("{name}: Failed to create MCP tool set: {e}"));
        let runner = weather_agent_runner(ts, &format!("TestMCPToolSetConfirmation/{name}")).await;

        let prompt = format!("what is the weather in {}?", tc.city);
        let mut count = 0;
        let mut confirm_call_id = None;
        check_stream(
            name,
            runner.run(&prompt),
            &tc.want,
            &mut count,
            &mut confirm_call_id,
        )
        .await;

        if let (Some(id), Some(confirmed)) = (confirm_call_id.clone(), tc.confirm) {
            let reply = Content {
                parts: vec![Part {
                    function_response: Some(FunctionResponse {
                        id,
                        name: REQUEST_CONFIRMATION_FUNCTION_CALL_NAME.into(),
                        response: obj(json!({"confirmed": confirmed})),
                        ..Default::default()
                    }),
                    ..Default::default()
                }],
                ..Default::default()
            };
            check_stream(
                name,
                runner.run_content(reply),
                &tc.want,
                &mut count,
                &mut confirm_call_id,
            )
            .await;
        }

        // Final check on the number of events.
        assert_eq!(count, tc.want.len(), "{name}: unexpected stream length");
    }
}

// parity: adapted: Go's nil provider and func(string, any) bool cases are None and Some(Arc<Fn(&str, &Value) -> bool>); the subtests run as a loop
#[tokio::test]
async fn test_new_tool_set_require_confirmation_provider_validation() {
    let tests: Vec<(&str, Option<ConfirmationProvider>)> = vec![
        ("Valid: Nil provider is allowed", None),
        (
            "Valid: Correct function signature",
            Some(Arc::new(|_: &str, _: &Value| true)),
        ),
    ];
    for (name, provider) in tests {
        let server = || ToolServer::new("test", "v1.0.0").weather("test", "test");
        let mut cfg = Config::new(McpTransport::Custom(in_memory(server)));
        cfg.require_confirmation_provider = provider;
        if let Err(e) = gadk::mcp::new(cfg) {
            panic!("{name}: unexpected error: {e}");
        }
    }
}

/// An object input schema for the tools Go registers with `args any` (go-sdk infers `{}`-like
/// schemas for them; the schema plays no part in these tests).
fn any_input_schema() -> Arc<JsonObject> {
    Arc::new(obj(json!({"type": "object"})))
}

/// The toolset's only tool, over a server with the single tool `def` answered by `result`.
async fn single_tool(def: McpToolDef, result: CallToolResult) -> Arc<dyn Tool> {
    let server = move || {
        let result = result.clone();
        ToolServer::new("test_server", "v1.0.0").tool(def.clone(), move |_, _| {
            let result = result.clone();
            async move { Ok(result) }
        })
    };
    let ts = new_toolset(in_memory(server));
    let tools = ts
        .tools(&inv_ctx())
        .await
        .unwrap_or_else(|e| panic!("Failed to get tools: {e}"));
    assert_eq!(tools.len(), 1, "Expected 1 tool, got {}", tools.len());
    tools.into_iter().next().unwrap()
}

#[tokio::test]
async fn test_mcp_tool_empty_text_response() {
    let tool = single_tool(
        McpToolDef::new("empty_tool", "returns empty response", any_input_schema()),
        CallToolResult::success(vec![ContentBlock::text("")]),
    )
    .await;

    let res = run_tool(&tool, tool_ctx(&inv_ctx(), None), json!({}))
        .await
        .unwrap_or_else(|e| panic!("Expected Run to succeed on empty text response, got: {e}"));
    assert_eq!(
        res.get("output"),
        Some(&json!("")),
        "Expected output to be empty string"
    );
}

// parity: adapted: rmcp carries Go's []byte payloads as their base64 text ("package example\n", [1 2 3 4], [1 2 3])
#[tokio::test]
async fn test_mcp_tool_non_text_content_round_trip() {
    let tool = single_tool(
        McpToolDef::new(
            "content_tool",
            "returns non-text content",
            any_input_schema(),
        ),
        CallToolResult::success(vec![
            ContentBlock::resource(
                ResourceContents::blob(
                    "cGFja2FnZSBleGFtcGxlCg==",
                    "repo://owner/project/main/file.go",
                )
                .with_mime_type("text/plain"),
            ),
            ContentBlock::resource_link(
                Resource::new("https://example.com/report.pdf", "report.pdf")
                    .with_mime_type("application/pdf")
                    .with_size(42),
            ),
            ContentBlock::image("AQIDBA==", "image/png"),
            ContentBlock::audio("AQID", "audio/wav"),
        ]),
    )
    .await;

    let got = run_tool(&tool, tool_ctx(&inv_ctx(), None), json!({}))
        .await
        .unwrap_or_else(|e| panic!("Run() failed: {e}"));

    let want = obj(json!({"output": "[MCP embedded resource: \
        uri=\"repo://owner/project/main/file.go\", mimeType=\"text/plain\"]\n\
        package example\n\
        [MCP resource link: uri=\"https://example.com/report.pdf\", mimeType=\"application/pdf\", \
        name=\"report.pdf\", size=42 bytes]\n\
        [MCP image: mimeType=\"image/png\", size=4 bytes]\n\
        [MCP audio: mimeType=\"audio/wav\", size=3 bytes]"}));
    assert_eq!(got, want, "Run() result mismatch");
}

/// Go `metaCapture`: the `_meta` an MCP server received on tool calls.
#[derive(Default)]
struct MetaCapture {
    called: bool,
    meta: JsonObject,
    metas: Vec<JsonObject>,
    /// The client's implementation info from the session's `initialize` handshake.
    client_info: Option<Value>,
}

type Captured = Arc<Mutex<MetaCapture>>;

/// Go `mcpClientMetaKeys`: the `_meta` entries the MCP client puts on every outgoing request.
/// They are not what a `MetadataProvider` contributed, so the tests filter them out before
/// comparing. go-sdk's are its three `MetaKey*` entries; rmcp's client (protocol revisions up to
/// `2025-11-25`) sends none of those and instead sets `progressToken` on every request.
const MCP_CLIENT_META_KEYS: [&str; 4] = [
    "progressToken",
    "io.modelcontextprotocol/protocolVersion",
    "io.modelcontextprotocol/clientInfo",
    "io.modelcontextprotocol/clientCapabilities",
];

/// Go `providerMeta`: the received `_meta` without the client's own entries.
fn provider_meta(meta: &JsonObject) -> JsonObject {
    let mut got = meta.clone();
    for k in MCP_CLIENT_META_KEYS {
        got.remove(k);
    }
    got
}

/// The `meta_server` of Go `startMetaEchoServer` / `TestMetadataProviderConfirmation`: one
/// tool, `get_weather`, that records each call's `_meta` and answers
/// `{"weather_summary": "sunny"}`.
fn meta_server(got: Captured) -> impl Fn() -> ToolServer + Send + Sync + 'static {
    move || {
        let got = got.clone();
        ToolServer::new("meta_server", "v1.0.0").tool(
            McpToolDef::new(TOOL_NAME, TOOL_DESCRIPTION, any_input_schema()),
            move |_params: CallToolRequestParams, context: RequestContext<RoleServer>| {
                let got = got.clone();
                async move {
                    let meta = context.meta.0.0.clone();
                    let client_info = context
                        .peer
                        .peer_info()
                        .map(|info| serde_json::to_value(&info.client_info).unwrap());
                    let mut got = got.lock().unwrap();
                    got.called = true;
                    got.meta = meta.clone();
                    got.metas.push(meta);
                    got.client_info = client_info;
                    let mut result = CallToolResult::success(vec![ContentBlock::text(
                        r#"{"weather_summary":"sunny"}"#,
                    )]);
                    result.structured_content = Some(json!({"weather_summary": "sunny"}));
                    Ok::<_, ErrorData>(result)
                }
            },
        )
    }
}

/// Go `startMetaEchoServer`: a toolset with `provider` over the meta server.
fn start_meta_echo_server(provider: Option<MetadataProvider>) -> (McpToolset, Captured) {
    let got = Captured::default();
    let mut cfg = Config::new(McpTransport::Custom(in_memory(meta_server(got.clone()))));
    cfg.metadata_provider = provider;
    let ts = gadk::mcp::new(cfg).unwrap_or_else(|e| panic!("mcptoolset.New() err = {e}"));
    (ts, got)
}

/// The toolset's only tool.
async fn only_tool(ts: &McpToolset, inv: &Context) -> Arc<dyn Tool> {
    let tools = ts
        .tools(inv)
        .await
        .unwrap_or_else(|e| panic!("Tools() err = {e}"));
    assert_eq!(
        tools.len(),
        1,
        "Tools() returned {} tools, want 1",
        tools.len()
    );
    tools.into_iter().next().unwrap()
}

/// Go `runSingleTool`: runs the toolset's only tool with `{"city": "Paris"}` in a tool context
/// of `inv`.
async fn run_single_tool(ts: &McpToolset, inv: &Context) -> Result<Map<String, Value>, ToolError> {
    let tool = only_tool(ts, inv).await;
    run_tool(&tool, tool_ctx(inv, None), json!({"city": "Paris"})).await
}

fn provider(
    f: impl Fn(&Context) -> Result<Option<Map<String, Value>>, BoxError> + Send + Sync + 'static,
) -> Option<MetadataProvider> {
    Some(Arc::new(f))
}

// parity: adapted: Go's subtests run as a loop; a nil map is None
#[tokio::test]
async fn test_metadata_provider() {
    let tests: Vec<(&str, Option<MetadataProvider>, JsonObject)> = vec![
        (
            "provider metadata reaches the server",
            provider(|_| Ok(Some(obj(json!({"trace_id": "abc-123", "tenant": "acme"}))))),
            obj(json!({"trace_id": "abc-123", "tenant": "acme"})),
        ),
        ("no provider configured sends no metadata", None, Map::new()),
        (
            "provider returning nil sends no metadata",
            provider(|_| Ok(None)),
            Map::new(),
        ),
        (
            "provider returning an empty map sends no metadata",
            provider(|_| Ok(Some(Map::new()))),
            Map::new(),
        ),
    ];
    for (name, provider, want_meta) in tests {
        let (ts, got) = start_meta_echo_server(provider);
        if let Err(e) = run_single_tool(&ts, &inv_ctx()).await {
            panic!("{name}: Run() err = {e}, want nil");
        }
        let got = got.lock().unwrap();
        assert!(got.called, "{name}: the MCP tool was never invoked");
        assert_eq!(
            provider_meta(&got.meta),
            want_meta,
            "{name}: server-side _meta mismatch"
        );
    }
}

/// Go `errors.New("no trace id in context")`, found again through `errors.Is`.
#[derive(Debug)]
struct NoTraceId;

impl std::fmt::Display for NoTraceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("no trace id in context")
    }
}

impl std::error::Error for NoTraceId {}

/// Go `errors.Is(err, target)` for a `NoTraceId`: walks the `source()` chain.
fn wraps_no_trace_id(err: &(dyn std::error::Error + 'static)) -> bool {
    let mut cur = Some(err);
    while let Some(e) = cur {
        if e.downcast_ref::<NoTraceId>().is_some() {
            return true;
        }
        cur = e.source();
    }
    false
}

#[tokio::test]
async fn test_metadata_provider_error() {
    let (ts, got) = start_meta_echo_server(provider(|_| Err(Box::new(NoTraceId))));

    let err = match run_single_tool(&ts, &inv_ctx()).await {
        Ok(_) => panic!("Run() err = nil, want the provider error"),
        Err(err) => err,
    };
    let e = mcp_error(&err);
    assert!(
        wraps_no_trace_id(e),
        "Run() err = {e}, want it to wrap {NoTraceId}"
    );
    assert!(
        e.to_string().contains("get_weather"),
        "Run() err = {e:?}, want it to name the tool"
    );
    assert!(
        !got.lock().unwrap().called,
        "the MCP tool ran despite the metadata provider failing"
    );
}

// parity: adapted: Go's subtests run as a loop; the mcp.MetaKey* constants are spelled out
#[tokio::test]
async fn test_metadata_provider_rejects_reserved_keys() {
    let tests: Vec<(&str, Value)> = vec![
        ("progressToken", json!("spoofed")),
        (
            "io.modelcontextprotocol/protocolVersion",
            json!("2025-06-18"),
        ),
        (
            "io.modelcontextprotocol/clientInfo",
            json!({"name": "spoofed", "version": "0.0.1"}),
        ),
        ("io.modelcontextprotocol/clientCapabilities", json!({})),
        ("io.modelcontextprotocol/custom", json!("spoofed")),
    ];
    for (key, val) in tests {
        let map = obj(json!({"trace_id": "abc-123", key: val}));
        let (ts, got) = start_meta_echo_server(provider(move |_| Ok(Some(map.clone()))));

        let err = match run_single_tool(&ts, &inv_ctx()).await {
            Ok(_) => panic!("Run() err = nil, want error for reserved key {key:?}"),
            Err(err) => err.to_string(),
        };
        assert!(
            err.contains(key),
            "Run() err = {err:?}, want it to name reserved key {key:?}"
        );
        assert!(
            err.contains("get_weather"),
            "Run() err = {err:?}, want it to name the tool"
        );
        assert!(
            !got.lock().unwrap().called,
            "{key}: the MCP tool ran despite the metadata provider returning a reserved key"
        );
    }
}

// parity: adapted: Go's subtests run as a loop
#[tokio::test]
async fn test_metadata_provider_allows_non_reserved_keys() {
    let tests: Vec<(&str, Value)> = vec![
        ("com.example/tenant", json!("acme")),
        ("io.modelcontextprotocolx", json!("custom")),
        ("ProgressToken", json!("tok-1")),
    ];
    for (key, val) in tests {
        let want_meta = obj(json!({key: val}));
        let answer = want_meta.clone();
        let (ts, got) = start_meta_echo_server(provider(move |_| Ok(Some(answer.clone()))));

        if let Err(e) = run_single_tool(&ts, &inv_ctx()).await {
            panic!("{key}: Run() err = {e}, want nil");
        }
        let got = got.lock().unwrap();
        assert!(got.called, "{key}: the MCP tool was never invoked");
        assert_eq!(
            provider_meta(&got.meta),
            want_meta,
            "{key}: server-side _meta mismatch"
        );
    }
}

// parity: adapted: rmcp's client puts progressToken, not go-sdk's MetaKey* entries, on every request's _meta, so that entry is checked and the adk-mcp-client info is checked where rmcp sends it (the initialize handshake)
#[tokio::test]
async fn test_metadata_provider_keeps_client_meta() {
    let (ts, got) =
        start_meta_echo_server(provider(|_| Ok(Some(obj(json!({"trace_id": "abc-123"}))))));
    if let Err(e) = run_single_tool(&ts, &inv_ctx()).await {
        panic!("Run() err = {e}");
    }
    let got = got.lock().unwrap();
    assert_eq!(
        provider_meta(&got.meta),
        obj(json!({"trace_id": "abc-123"})),
        "provider _meta mismatch"
    );
    assert!(
        got.meta.contains_key("progressToken"),
        "received _meta is missing the client's own \"progressToken\", got keys {:?}",
        got.meta.keys().collect::<Vec<_>>()
    );
    let want_client_info = json!({
        "name": "adk-mcp-client",
        "version": env!("CARGO_PKG_VERSION"),
    });
    assert_eq!(
        got.client_info.as_ref(),
        Some(&want_client_info),
        "received client info mismatch"
    );
}

// parity: adapted: a Rust provider returns an owned map, so the reused map is shared through a mutex and cloned by the provider; the test checks it is unchanged after the call
#[tokio::test]
async fn test_metadata_provider_map_is_copied() {
    let reused = Arc::new(Mutex::new(obj(json!({"trace_id": "abc-123"}))));
    let shared = reused.clone();
    let (ts, got) =
        start_meta_echo_server(provider(move |_| Ok(Some(shared.lock().unwrap().clone()))));
    if let Err(e) = run_single_tool(&ts, &inv_ctx()).await {
        panic!("Run() err = {e}");
    }
    assert_eq!(
        *reused.lock().unwrap(),
        obj(json!({"trace_id": "abc-123"})),
        "the provider's own map was modified"
    );
    // The provider hands over an owned map, so the check above holds by construction in Rust;
    // also check the map reached the server unchanged.
    assert_eq!(
        provider_meta(&got.lock().unwrap().meta),
        obj(json!({"trace_id": "abc-123"})),
        "provider _meta mismatch"
    );
}

#[tokio::test]
async fn test_metadata_provider_per_call() {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let (ts, got) = start_meta_echo_server(provider(move |_| {
        let n = counter.fetch_add(1, Ordering::SeqCst) + 1;
        Ok(Some(obj(json!({"trace_id": format!("call-{n}")}))))
    }));

    let inv = inv_ctx();
    let tool = only_tool(&ts, &inv).await;
    for i in 1..=2 {
        if let Err(e) = run_tool(&tool, tool_ctx(&inv, None), json!({"city": "Paris"})).await {
            panic!("Run() call {i} err = {e}");
        }
    }

    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "provider called the wrong number of times"
    );
    let got = got.lock().unwrap();
    assert_eq!(
        got.metas.len(),
        2,
        "server received the wrong number of calls"
    );
    let got_metas: Vec<JsonObject> = got.metas.iter().map(provider_meta).collect();
    let want_metas = vec![
        obj(json!({"trace_id": "call-1"})),
        obj(json!({"trace_id": "call-2"})),
    ];
    assert_eq!(
        got_metas, want_metas,
        "server-side _meta across calls mismatch"
    );
}

// parity: adapted: gadk's Context carries no request-scoped values (Go context.WithValue), so the provider derives the trace id from the tool context it is handed: the invocation id and the call's function call id
#[tokio::test]
async fn test_metadata_provider_receives_tool_context() {
    let seen: Arc<Mutex<Option<(String, String)>>> = Arc::default();
    let record = seen.clone();
    let (ts, got) = start_meta_echo_server(provider(move |ctx| {
        let invocation_id = ctx.invocation_id().to_string();
        let call_id = ctx.function_call_id();
        *record.lock().unwrap() = Some((invocation_id.clone(), call_id));
        Ok(Some(obj(json!({"trace_id": invocation_id}))))
    }));
    let inv = Context::builder().invocation_id("abc-123").build();
    let tool = only_tool(&ts, &inv).await;
    let call_ctx = tool_ctx(&inv, None);
    let want_call_id = call_ctx.function_call_id();
    if let Err(e) = run_tool(&tool, call_ctx, json!({"city": "Paris"})).await {
        panic!("Run() err = {e}");
    }
    let seen = seen.lock().unwrap().clone().expect("provider not called");
    assert_eq!(
        seen.0,
        inv.invocation_id(),
        "provider got the wrong invocation ID"
    );
    assert_eq!(seen.1, want_call_id, "provider got the wrong tool context");
    assert_eq!(
        provider_meta(&got.lock().unwrap().meta),
        obj(json!({"trace_id": "abc-123"})),
        "server-side _meta mismatch"
    );
}

/// The expected outcome of a `TestMetadataProviderConfirmation` run.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Want {
    ConfirmationRequired,
    ConfirmationRejected,
    Ok,
}

/// One `TestMetadataProviderConfirmation` case.
struct MetaConfirmationCase {
    name: &'static str,
    confirmation: Option<ToolConfirmation>,
    provider_err: Option<&'static str>,
    want: Want,
    want_provider_called: bool,
    want_server_called: bool,
}

// parity: adapted: Go's subtests run as a loop; errors.Is(err, tool.ErrConfirmation*) matches the ToolError variant
#[tokio::test]
async fn test_metadata_provider_confirmation() {
    let tests = vec![
        MetaConfirmationCase {
            name: "no confirmation",
            confirmation: None,
            provider_err: Some("provider ran before confirmation"),
            want: Want::ConfirmationRequired,
            want_provider_called: false,
            want_server_called: false,
        },
        MetaConfirmationCase {
            name: "rejected confirmation",
            confirmation: Some(ToolConfirmation {
                confirmed: false,
                ..Default::default()
            }),
            provider_err: Some("provider ran on rejected confirmation"),
            want: Want::ConfirmationRejected,
            want_provider_called: false,
            want_server_called: false,
        },
        MetaConfirmationCase {
            name: "approved confirmation",
            confirmation: Some(ToolConfirmation {
                confirmed: true,
                ..Default::default()
            }),
            provider_err: None,
            want: Want::Ok,
            want_provider_called: true,
            want_server_called: true,
        },
    ];
    for tc in tests {
        let MetaConfirmationCase {
            name,
            confirmation,
            provider_err,
            want,
            want_provider_called,
            want_server_called,
        } = tc;
        let got = Captured::default();
        let provider_called = Arc::new(AtomicUsize::new(0));
        let called = provider_called.clone();
        let mut cfg = Config::new(McpTransport::Custom(in_memory(meta_server(got.clone()))));
        cfg.require_confirmation = true;
        cfg.metadata_provider = provider(move |_| {
            called.fetch_add(1, Ordering::SeqCst);
            match provider_err {
                Some(msg) => Err(msg.into()),
                None => Ok(Some(obj(json!({"trace_id": "abc-123"})))),
            }
        });
        let ts = gadk::mcp::new(cfg).unwrap_or_else(|e| panic!("mcptoolset.New() err = {e}"));

        let inv = inv_ctx();
        let tool = only_tool(&ts, &inv).await;
        let res = run_tool(
            &tool,
            tool_ctx(&inv, confirmation),
            json!({"city": "Paris"}),
        )
        .await;
        let got_want = match &res {
            Ok(_) => Want::Ok,
            Err(ToolError::ConfirmationRequired) => Want::ConfirmationRequired,
            Err(ToolError::ConfirmationRejected) => Want::ConfirmationRejected,
            Err(e) => panic!("{name}: Run() err = {e}, want {want:?}"),
        };
        assert_eq!(got_want, want, "{name}: Run() result = {res:?}");
        assert_eq!(
            provider_called.load(Ordering::SeqCst) > 0,
            want_provider_called,
            "{name}: providerCalled"
        );
        let got = got.lock().unwrap();
        assert_eq!(got.called, want_server_called, "{name}: server called");
        if want_server_called {
            assert_eq!(
                provider_meta(&got.meta),
                obj(json!({"trace_id": "abc-123"})),
                "{name}: server-side _meta mismatch"
            );
        }
    }
}
