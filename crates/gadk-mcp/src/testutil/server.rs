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

//! [`ToolServer`]: an rmcp server with fixed tools, and a weather server.

use std::borrow::Cow;
use std::future::Future;
use std::sync::{Arc, Mutex};

use futures::future::BoxFuture;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    JsonObject, ListToolsResult, PaginatedRequestParams, ProtocolVersion, ServerCapabilities,
    ServerConfig, Tool,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServerHandler};
use serde_json::{Value, json};

use super::lock;

/// What a [`ToolHandler`] returns.
pub type ToolFuture = BoxFuture<'static, Result<CallToolResult, ErrorData>>;

/// A tool's handler: the call's parameters and its request context (`context.meta` holds the
/// request's `_meta`; over HTTP, `context.extensions` holds the request's
/// `http::request::Parts`).
pub type ToolHandler =
    Arc<dyn Fn(CallToolRequestParams, RequestContext<RoleServer>) -> ToolFuture + Send + Sync>;

/// A server whose `tools/list` returns the added tools in order (one page) and whose `tools/call`
/// runs the matching handler. Cheap to clone; clones share the tools.
///
/// It speaks the protocol revisions that still have an `initialize` handshake (up to
/// `2025-11-25`), so streamable HTTP keeps stateful sessions.
#[derive(Clone)]
pub struct ToolServer {
    info: Implementation,
    tools: Arc<Mutex<Vec<(Tool, ToolHandler)>>>,
}

impl ToolServer {
    /// A server named `name` at `version`, without tools.
    pub fn new(name: &str, version: &str) -> Self {
        Self {
            info: Implementation::new(name, version),
            tools: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Adds `tool`, answered by `handler`.
    pub fn tool<H, Fut>(self, tool: Tool, handler: H) -> Self
    where
        H: Fn(CallToolRequestParams, RequestContext<RoleServer>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<CallToolResult, ErrorData>> + Send + 'static,
    {
        let handler: ToolHandler =
            Arc::new(move |params, context| Box::pin(handler(params, context)));
        lock(&self.tools).push((tool, handler));
        self
    }

    /// Adds a weather tool as `name` (an empty `description` is omitted): the input schema takes a
    /// `city`, the output schema a `weather_summary`; it answers
    /// `{"weather_summary": "Today in \"<city>\" is sunny"}` as structured content, with that
    /// JSON's text as the content.
    pub fn weather(self, name: &str, description: &str) -> Self {
        let description = (!description.is_empty()).then(|| Cow::Owned(description.to_owned()));
        let tool = Tool::new_with_raw(name.to_owned(), description, object(weather_input_schema()))
            .with_raw_output_schema(Arc::new(object(weather_output_schema())));
        self.tool(tool, |params: CallToolRequestParams, _| async move {
            Ok(weather_call(params.arguments.unwrap_or_default()))
        })
    }

    fn tool_list(&self) -> Vec<Tool> {
        lock(&self.tools)
            .iter()
            .map(|(tool, _)| tool.clone())
            .collect()
    }

    fn handler(&self, name: &str) -> Option<ToolHandler> {
        lock(&self.tools)
            .iter()
            .find(|(tool, _)| tool.name == name)
            .map(|(_, handler)| handler.clone())
    }
}

impl ServerHandler for ToolServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(self.info.clone())
            .with_protocol_version(ProtocolVersion::LATEST_WITH_INITIALIZE)
    }

    fn supported_protocol_versions(&self) -> Cow<'static, [ProtocolVersion]> {
        Cow::Borrowed(ProtocolVersion::known_up_to(
            &ProtocolVersion::LATEST_WITH_INITIALIZE,
        ))
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(self.tool_list()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        // An unknown tool is a JSON-RPC invalid-params error.
        let Some(handler) = self.handler(&request.name) else {
            return Err(ErrorData::invalid_params(
                format!("unknown tool {}", gadk_agent::go_quote(&request.name)),
                None,
            ));
        };
        handler(request, context).await.map(CallToolResponse::from)
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        lock(&self.tools)
            .iter()
            .find(|(tool, _)| tool.name == name)
            .map(|(tool, _)| tool.clone())
    }
}

/// The weather server: `weather_server` `v1.0.0` with
/// `.weather("get_weather", "returns weather in the given city")`.
pub fn weather_server() -> ToolServer {
    ToolServer::new("weather_server", "v1.0.0")
        .weather("get_weather", "returns weather in the given city")
}

/// The input schema of the weather tool: a required string `city` ("city name"), as the recorded
/// Gemini requests carry it.
fn weather_input_schema() -> Value {
    json!({
        "additionalProperties": false,
        "properties": {"city": {"description": "city name", "type": "string"}},
        "required": ["city"],
        "type": "object",
    })
}

/// The output schema of the weather tool: a required string `weather_summary` ("weather summary in
/// the given city").
fn weather_output_schema() -> Value {
    json!({
        "additionalProperties": false,
        "properties": {
            "weather_summary": {
                "description": "weather summary in the given city",
                "type": "string",
            },
        },
        "required": ["weather_summary"],
        "type": "object",
    })
}

fn object(v: Value) -> JsonObject {
    match v {
        Value::Object(map) => map,
        _ => JsonObject::new(),
    }
}

/// The weather tool's handler: validates the arguments against the input schema (failures are an
/// `isError` result), then answers the output as structured content and as its JSON text.
fn weather_call(args: JsonObject) -> CallToolResult {
    if let Err(e) = validate_weather_input(&args) {
        return without_is_error(CallToolResult::error(vec![ContentBlock::text(format!(
            "validating \"arguments\": {e}"
        ))]));
    }
    let city = args.get("city").and_then(Value::as_str).unwrap_or_default();
    let summary = format!("Today in {} is sunny", gadk_agent::go_quote(city));
    let output = json!({"weather_summary": summary});
    let mut result = CallToolResult::success(vec![ContentBlock::text(go_json_text(&output))]);
    result.structured_content = Some(output);
    without_is_error(result)
}

/// Clears `isError` unless it is true, so it is only sent when true.
fn without_is_error(mut result: CallToolResult) -> CallToolResult {
    if result.is_error != Some(true) {
        result.is_error = None;
    }
    result
}

/// Validates the arguments against the input schema, with the schema validator's error texts:
/// property types, then `additionalProperties: false`, then `required`.
fn validate_weather_input(args: &JsonObject) -> Result<(), String> {
    if let Some(city) = args.get("city")
        && !city.is_string()
    {
        return Err(format!(
            "validating root: validating /properties/city: type: {city} has type {}, want {}",
            gadk_agent::go_quote(json_type(city)),
            gadk_agent::go_quote("string"),
        ));
    }
    let extra: Vec<String> = args
        .keys()
        .filter(|k| k.as_str() != "city")
        .map(|k| gadk_agent::go_quote(k))
        .collect();
    if !extra.is_empty() {
        return Err(format!(
            "validating root: unexpected additional properties [{}]",
            extra.join(" ")
        ));
    }
    if !args.contains_key("city") {
        return Err(format!(
            "validating root: required: missing properties: [{}]",
            gadk_agent::go_quote("city")
        ));
    }
    Ok(())
}

fn json_type(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(n) if n.is_i64() || n.is_u64() => "integer",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// The compact JSON text of `v` with `<`, `>`, `&`, U+2028 and U+2029 escaped as `\u003c`,
/// `\u003e`, `\u0026`, `\u2028` and `\u2029`. Those characters only occur inside strings, so
/// escaping them in the whole text is exact.
fn go_json_text(v: &Value) -> String {
    let mut out = String::new();
    for c in v.to_string().chars() {
        match c {
            '<' => out.push_str("\\u003c"),
            '>' => out.push_str("\\u003e"),
            '&' => out.push_str("\\u0026"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::sync::Mutex;

    use pretty_assertions::assert_eq;
    use rmcp::model::{ErrorCode, MetaObject, RequestMetaObject};
    use rmcp::service::RunningService;
    use rmcp::{RoleClient, ServiceError};

    use super::*;
    use crate::config::default_client_info;
    use crate::testutil::in_memory;

    async fn connect(server: ToolServer) -> RunningService<RoleClient, rmcp::model::ClientConfig> {
        in_memory(move || server.clone())
            .connect(default_client_info())
            .await
            .unwrap()
    }

    fn args(v: Value) -> JsonObject {
        object(v)
    }

    async fn call(server: ToolServer, arguments: Option<Value>) -> CallToolResult {
        let client = connect(server).await;
        let mut params = CallToolRequestParams::new("get_weather");
        params.arguments = arguments.map(args);
        let result = client.call_tool(params).await.unwrap();
        client.cancel().await.unwrap();
        result
    }

    fn text_of(result: &CallToolResult) -> String {
        match result.content.as_slice() {
            [block] => block.as_text().unwrap().text.clone(),
            other => panic!("want one content block, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn weather_server_lists_get_weather_with_go_sdk_schemas() {
        let client = connect(weather_server()).await;
        let info = client.peer_info().unwrap();
        let server_info = info.server_info.as_ref().unwrap();
        assert_eq!(server_info.name, "weather_server");
        assert_eq!(server_info.version, "v1.0.0");
        assert_eq!(info.protocol_version, ProtocolVersion::V_2025_11_25);
        assert!(info.capabilities.tools.is_some());

        let tools = client.list_tools(None).await.unwrap();
        assert_eq!(tools.next_cursor, None);
        let [tool] = tools.tools.as_slice() else {
            panic!("want one tool, got {:?}", tools.tools);
        };
        assert_eq!(
            serde_json::to_value(tool).unwrap(),
            json!({
                "name": "get_weather",
                "description": "returns weather in the given city",
                "inputSchema": weather_input_schema(),
                "outputSchema": weather_output_schema(),
            })
        );
        // Byte-identical to the declaration in the TestMCPToolSet*.httprr request bodies.
        assert_eq!(
            serde_json::to_string(&tool.input_schema).unwrap(),
            r#"{"additionalProperties":false,"properties":{"city":{"description":"city name","type":"string"}},"required":["city"],"type":"object"}"#
        );
        assert_eq!(
            serde_json::to_string(tool.output_schema.as_ref().unwrap()).unwrap(),
            r#"{"additionalProperties":false,"properties":{"weather_summary":{"description":"weather summary in the given city","type":"string"}},"required":["weather_summary"],"type":"object"}"#
        );
        client.cancel().await.unwrap();
    }

    #[tokio::test]
    async fn weather_answers_structured_content_and_its_json_text() {
        let result = call(weather_server(), Some(json!({"city": "london"}))).await;
        assert_eq!(
            result.structured_content,
            Some(json!({"weather_summary": "Today in \"london\" is sunny"}))
        );
        assert_eq!(
            text_of(&result),
            r#"{"weather_summary":"Today in \"london\" is sunny"}"#
        );
        assert_eq!(result.is_error, None);
    }

    #[tokio::test]
    async fn weather_text_escapes_like_go_json_marshal() {
        let result = call(weather_server(), Some(json!({"city": "<a&b>\u{2028}é"}))).await;
        assert_eq!(
            result.structured_content,
            Some(json!({"weather_summary": "Today in \"<a&b>\\u2028é\" is sunny"}))
        );
        assert_eq!(
            text_of(&result),
            r#"{"weather_summary":"Today in \"\u003ca\u0026b\u003e\\u2028é\" is sunny"}"#
        );
    }

    #[tokio::test]
    async fn weather_rejects_arguments_that_fail_the_input_schema() {
        for (arguments, want) in [
            (
                None,
                r#"validating "arguments": validating root: required: missing properties: ["city"]"#,
            ),
            (
                Some(json!({"city": "Paris", "x": 1})),
                r#"validating "arguments": validating root: unexpected additional properties ["x"]"#,
            ),
            (
                Some(json!({"city": 1})),
                r#"validating "arguments": validating root: validating /properties/city: type: 1 has type "integer", want "string""#,
            ),
        ] {
            let result = call(weather_server(), arguments).await;
            assert_eq!(result.is_error, Some(true));
            assert_eq!(result.structured_content, None);
            assert_eq!(text_of(&result), want);
        }
    }

    #[tokio::test]
    async fn weather_with_empty_description_omits_it() {
        let client = connect(ToolServer::new("test_server", "1").weather("get_weather", "")).await;
        let tools = client.list_tools(None).await.unwrap().tools;
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].description, None);
        assert!(
            !serde_json::to_string(&tools[0])
                .unwrap()
                .contains("description\":\"\"")
        );
        client.cancel().await.unwrap();
    }

    #[tokio::test]
    async fn unknown_tool_is_an_invalid_params_error() {
        let client = connect(weather_server()).await;
        let err = client
            .call_tool(CallToolRequestParams::new("nope"))
            .await
            .unwrap_err();
        let ServiceError::McpError(data) = err else {
            panic!("want an MCP error, got {err:?}");
        };
        assert_eq!(data.code, ErrorCode::INVALID_PARAMS);
        assert_eq!(data.message, r#"unknown tool "nope""#);
        client.cancel().await.unwrap();
    }

    #[tokio::test]
    async fn tools_list_in_order_clones_share_them_and_handlers_see_the_call() {
        /// Each call's arguments and request `_meta`.
        type Seen = Vec<(Option<JsonObject>, JsonObject)>;
        let seen: Arc<Mutex<Seen>> = Arc::default();
        let server = ToolServer::new("test_server", "1");
        let seen2 = seen.clone();
        // Tools added through a clone show up on the original.
        let _ = server
            .clone()
            .tool(
                Tool::new("b", "second", object(json!({"type": "object"}))),
                move |params: CallToolRequestParams, context: RequestContext<RoleServer>| {
                    let seen = seen2.clone();
                    async move {
                        seen.lock()
                            .unwrap()
                            .push((params.arguments, context.meta.0.0.clone()));
                        Ok(CallToolResult::success(vec![ContentBlock::text("from b")]))
                    }
                },
            )
            .tool(
                Tool::new("a", "first", object(json!({"type": "object"}))),
                |_, _| async { Err(ErrorData::internal_error("boom", None)) },
            );
        let client = connect(server).await;
        let names: Vec<_> = client
            .list_tools(None)
            .await
            .unwrap()
            .tools
            .into_iter()
            .map(|t| t.name.into_owned())
            .collect();
        assert_eq!(names, ["b", "a"]);

        let mut params = CallToolRequestParams::new("b").with_arguments(args(json!({"q": 1})));
        params.meta = Some(RequestMetaObject(MetaObject(args(json!({"k": "v"})))));
        let result = client.call_tool(params).await.unwrap();
        assert_eq!(text_of(&result), "from b");
        let seen = seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].0, Some(args(json!({"q": 1}))));
        assert_eq!(seen[0].1.get("k"), Some(&json!("v")));

        let err = client
            .call_tool(CallToolRequestParams::new("a"))
            .await
            .unwrap_err();
        assert!(
            matches!(err, ServiceError::McpError(ref e) if e.message == "boom"),
            "{err:?}"
        );
        client.cancel().await.unwrap();
    }
}
