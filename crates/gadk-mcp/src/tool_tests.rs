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

//! Rust-only unit tests of `McpTool` against a fake `McpClient`.
//!
//! MCP content is mostly single plain text blocks; `format_mcp_content`'s labels and joining rules
//! are tested in `parity_tests/tool_mcptoolset_tool.rs`, and one `isError` case checks that its text becomes the details.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use gadk_agent::tool::{ConfirmationProvider, RunnableTool, Tool, ToolOriginKind};
use gadk_agent::{Context, ContextDelta, ToolError};
use gadk_auth::AuthContext;
use gadk_session::ToolConfirmation;
use pretty_assertions::assert_eq;
use rmcp::model::{CallToolRequestParams, CallToolResult, ContentBlock, Tool as McpToolDef};
use serde_json::{Map, Value, json};
use tokio_util::sync::CancellationToken;

use super::McpTool;
use crate::client::{CallContext, McpClient};
use crate::config::MetadataProvider;
use crate::error::Error;

const HINT: &str = "Please approve or reject the tool call get_weather() by responding with a FunctionResponse with an expected ToolConfirmation payload.";

/// Records every `call_tool` (its context and params) and answers from a script; an empty
/// script answers a single text block `"ok"`.
#[derive(Default)]
struct FakeClient {
    calls: Mutex<Vec<(CallContext, CallToolRequestParams)>>,
    script: Mutex<VecDeque<Result<CallToolResult, Error>>>,
}

impl FakeClient {
    fn answering(results: Vec<Result<CallToolResult, Error>>) -> Arc<Self> {
        Arc::new(Self {
            calls: Mutex::default(),
            script: Mutex::new(results.into()),
        })
    }

    fn calls(&self) -> Vec<(CallContext, CallToolRequestParams)> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl McpClient for FakeClient {
    async fn call_tool(
        &self,
        cx: &CallContext,
        params: CallToolRequestParams,
    ) -> Result<CallToolResult, Error> {
        self.calls.lock().unwrap().push((cx.clone(), params));
        self.script
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Ok(CallToolResult::success(vec![ContentBlock::text("ok")])))
    }

    async fn list_tools(&self, _cx: &CallContext) -> Result<Vec<McpToolDef>, Error> {
        Ok(Vec::new())
    }
}

fn input_schema() -> Map<String, Value> {
    json!({
        "type": "object",
        "properties": {"city": {"type": "string", "description": "the city"}},
        "required": ["city"],
        "additionalProperties": false
    })
    .as_object()
    .unwrap()
    .clone()
}

fn weather_tool() -> McpToolDef {
    McpToolDef::new(
        "get_weather",
        "returns weather in the given city",
        Arc::new(input_schema()),
    )
}

fn mcp_tool(client: Arc<FakeClient>) -> McpTool {
    McpTool::new(weather_tool(), client, false, None, None)
}

/// A tool context with actions, function call id `fc-1` and the given confirmation.
fn tool_ctx(confirmation: Option<ToolConfirmation>) -> Context {
    tool_ctx_with_token(confirmation, CancellationToken::new())
}

fn tool_ctx_with_token(
    confirmation: Option<ToolConfirmation>,
    token: CancellationToken,
) -> Context {
    Context::builder()
        .invocation_id("inv-1")
        .cancellation_token(token)
        .build()
        .with_delta(ContextDelta {
            function_call_id: Some("fc-1".into()),
            tool_confirmation: Some(confirmation),
            actions: Some(Default::default()),
            ..Default::default()
        })
}

fn confirmation(confirmed: bool) -> Option<ToolConfirmation> {
    Some(ToolConfirmation {
        confirmed,
        ..Default::default()
    })
}

fn args() -> Value {
    json!({"city": "Paris"})
}

/// The crate error behind a `ToolError::Execution`.
fn execution_source(err: &ToolError) -> &Error {
    match err {
        ToolError::Execution(source) => source
            .downcast_ref::<Error>()
            .expect("an Execution source is a gadk_mcp::Error"),
        other => panic!("want ToolError::Execution, got {other:?}"),
    }
}

/// A metadata provider that counts its calls and answers `answer()`.
fn counting_metadata(
    answer: impl Fn() -> Result<Option<Map<String, Value>>, crate::BoxError> + Send + Sync + 'static,
) -> (MetadataProvider, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let provider: MetadataProvider = Arc::new(move |_ctx: &Context| {
        counter.fetch_add(1, Ordering::SeqCst);
        answer()
    });
    (provider, calls)
}

fn meta_map(v: Value) -> Map<String, Value> {
    v.as_object().unwrap().clone()
}

// The analytics origin: Go classifies a `tool/mcptoolset` tool as `MCP`.
#[test]
fn mcp_tool_is_an_mcp_tool() {
    let t = mcp_tool(FakeClient::answering(vec![]));
    assert_eq!(t.origin_kind(), ToolOriginKind::Mcp);
    assert!(t.origin_agent().is_none());
    assert!(t.unwrap().is_none());
}

// R3: name, description and the declaration with the schemas passed through unchanged.
#[test]
fn declaration_passes_the_mcp_schemas_through() {
    let output = json!({
        "type": "object",
        "properties": {"weather_summary": {"type": "string"}},
        "required": ["weather_summary"]
    });
    let tool = McpTool::new(
        weather_tool().with_raw_output_schema(Arc::new(meta_map(output.clone()))),
        FakeClient::answering(vec![]),
        false,
        None,
        None,
    );
    assert_eq!(tool.name(), "get_weather");
    assert_eq!(tool.description(), "returns weather in the given city");
    assert!(!tool.is_long_running());
    assert!(tool.as_runnable().is_some());
    assert!(tool.as_any().downcast_ref::<McpTool>().is_some());
    let decl = tool.declaration().expect("an MCP tool has a declaration");
    assert_eq!(decl.name, "get_weather");
    assert_eq!(decl.description, "returns weather in the given city");
    assert_eq!(
        decl.parameters_json_schema,
        Some(Value::Object(input_schema()))
    );
    assert_eq!(decl.response_json_schema, Some(output));
    assert_eq!(decl.parameters, None);
    assert_eq!(decl.response, None);
}

// R3: no outputSchema → no response schema; no description → empty.
#[test]
fn declaration_without_output_schema_or_description() {
    let mut def = weather_tool();
    def.description = None;
    let tool = McpTool::new(def, FakeClient::answering(vec![]), false, None, None);
    assert_eq!(tool.description(), "");
    let decl = tool.declaration().unwrap();
    assert_eq!(decl.description, "");
    assert_eq!(
        decl.parameters_json_schema,
        Some(Value::Object(input_schema()))
    );
    assert_eq!(decl.response_json_schema, None);
}

// R6: the call carries the tool name and arguments, no `_meta`, and the context's identity and
// cancellation token; a text result becomes {"output": text}.
#[tokio::test]
async fn run_calls_the_tool_and_returns_text_output() {
    let client =
        FakeClient::answering(vec![Ok(CallToolResult::success(vec![ContentBlock::text(
            "sunny",
        )]))]);
    let tool = mcp_tool(client.clone());
    let token = CancellationToken::new();
    let ctx = tool_ctx_with_token(None, token.clone());

    let got = tool
        .as_runnable()
        .unwrap()
        .run(ctx.clone(), args())
        .await
        .unwrap();
    assert_eq!(Value::Object(got), json!({"output": "sunny"}));

    let calls = client.calls();
    assert_eq!(calls.len(), 1);
    let (cx, params) = &calls[0];
    assert_eq!(params.name, "get_weather");
    assert_eq!(params.arguments, Some(meta_map(args())));
    assert_eq!(params.meta, None);
    assert_eq!(cx.auth, AuthContext::from(&ctx));
    assert!(!cx.cancel.is_cancelled());
    token.cancel();
    assert!(
        cx.cancel.is_cancelled(),
        "the call runs with the invocation's token"
    );
}

// R6: a non-object `args` is sent as no arguments.
#[tokio::test]
async fn run_with_null_args_sends_no_arguments() {
    let client = FakeClient::answering(vec![]);
    let tool = mcp_tool(client.clone());
    tool.as_runnable()
        .unwrap()
        .run(tool_ctx(None), Value::Null)
        .await
        .unwrap();
    assert_eq!(client.calls()[0].1.arguments, None);
}

// R6: structuredContent wins over content.
#[tokio::test]
async fn run_returns_structured_content() {
    let structured = json!({"weather_summary": "Today in \"Paris\" is sunny"});
    let client = FakeClient::answering(vec![Ok(CallToolResult::structured(structured.clone()))]);
    let got = mcp_tool(client).run(tool_ctx(None), args()).await.unwrap();
    assert_eq!(Value::Object(got), json!({"output": structured}));
}

// R6/P4: structuredContent JSON null counts as absent (Go `StructuredContent != nil`).
#[tokio::test]
async fn run_null_structured_content_falls_back_to_text() {
    let mut result = CallToolResult::success(vec![ContentBlock::text("plain")]);
    result.structured_content = Some(Value::Null);
    let client = FakeClient::answering(vec![Ok(result)]);
    let got = mcp_tool(client).run(tool_ctx(None), args()).await.unwrap();
    assert_eq!(Value::Object(got), json!({"output": "plain"}));
}

// R6: empty content without structured content → {"output": ""}.
#[tokio::test]
async fn run_empty_content_is_empty_text_output() {
    let client = FakeClient::answering(vec![Ok(CallToolResult::success(vec![]))]);
    let got = mcp_tool(client).run(tool_ctx(None), args()).await.unwrap();
    assert_eq!(Value::Object(got), json!({"output": ""}));
}

// R6: isError → "Tool execution failed." plus " Details: <formatted>" when non-empty.
#[tokio::test]
async fn run_is_error_texts() {
    let client = FakeClient::answering(vec![
        Ok(CallToolResult::error(vec![ContentBlock::text(
            "city not found",
        )])),
        Ok(CallToolResult::error(vec![])),
        Ok(CallToolResult::error(vec![
            ContentBlock::text("city not found"),
            ContentBlock::image("aGk=", "image/png"),
        ])),
    ]);
    let tool = mcp_tool(client);
    let err = tool.run(tool_ctx(None), args()).await.unwrap_err();
    assert_eq!(
        err.to_string(),
        "Tool execution failed. Details: city not found"
    );
    assert!(matches!(execution_source(&err), Error::ToolExecution(_)));

    let err = tool.run(tool_ctx(None), args()).await.unwrap_err();
    assert_eq!(err.to_string(), "Tool execution failed.");

    // The details are `format_mcp_content`'s text: a label on its own line.
    let err = tool.run(tool_ctx(None), args()).await.unwrap_err();
    assert_eq!(
        err.to_string(),
        "Tool execution failed. Details: city not found\n[MCP image: mimeType=\"image/png\", size=2 bytes]"
    );
}

// R6: a call error is wrapped with the tool name; the source chain keeps the client's error.
#[tokio::test]
async fn run_call_error_names_the_tool() {
    let client = FakeClient::answering(vec![Err(Error::ConnectionClosed)]);
    let err = mcp_tool(client)
        .run(tool_ctx(None), args())
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        r#"failed to call MCP tool "get_weather" with err: connection closed"#
    );
    let source = execution_source(&err);
    assert!(matches!(source, Error::CallTool { .. }));
    assert!(source.is_connection_closed());
}

// R4: the static flag requests confirmation with Go's hint, sets skip_summarization and returns
// the bare sentinel without calling the server or the metadata provider.
#[tokio::test]
async fn run_requires_confirmation() {
    let client = FakeClient::answering(vec![]);
    let (metadata, meta_calls) = counting_metadata(|| Ok(None));
    let tool = McpTool::new(weather_tool(), client.clone(), true, None, Some(metadata));
    let ctx = tool_ctx(None);

    let err = tool.run(ctx.clone(), args()).await.unwrap_err();
    assert!(matches!(err, ToolError::ConfirmationRequired), "{err:?}");
    let actions = ctx.actions();
    assert_eq!(
        actions.requested_tool_confirmations.get("fc-1"),
        Some(&ToolConfirmation {
            hint: HINT.to_string(),
            confirmed: false,
            payload: None,
        })
    );
    assert!(actions.skip_summarization);
    assert!(client.calls().is_empty());
    assert_eq!(meta_calls.load(Ordering::SeqCst), 0);
}

// R4: request_confirmation's error is returned as is (Go returns it).
#[tokio::test]
async fn run_confirmation_without_function_call_id_fails() {
    let client = FakeClient::answering(vec![]);
    let tool = McpTool::new(weather_tool(), client.clone(), true, None, None);
    let ctx = Context::builder().build();
    let err = tool.run(ctx, args()).await.unwrap_err();
    assert!(matches!(err, ToolError::InvalidArgument(_)), "{err:?}");
    assert!(client.calls().is_empty());
}

// R4: a rejected confirmation returns the bare sentinel; nothing else runs.
#[tokio::test]
async fn run_rejected_confirmation() {
    let client = FakeClient::answering(vec![]);
    let (metadata, meta_calls) = counting_metadata(|| Ok(None));
    let provider_calls = Arc::new(AtomicUsize::new(0));
    let counter = provider_calls.clone();
    let provider: ConfirmationProvider = Arc::new(move |_name: &str, _args: &Value| {
        counter.fetch_add(1, Ordering::SeqCst);
        true
    });
    let tool = McpTool::new(
        weather_tool(),
        client.clone(),
        true,
        Some(provider),
        Some(metadata),
    );
    let ctx = tool_ctx(confirmation(false));

    let err = tool.run(ctx.clone(), args()).await.unwrap_err();
    assert!(matches!(err, ToolError::ConfirmationRejected), "{err:?}");
    assert!(client.calls().is_empty());
    assert_eq!(meta_calls.load(Ordering::SeqCst), 0);
    assert_eq!(provider_calls.load(Ordering::SeqCst), 0);
    assert!(ctx.actions().requested_tool_confirmations.is_empty());
}

// R4/R5: an approved confirmation proceeds to the server; the metadata provider runs then.
#[tokio::test]
async fn run_approved_confirmation_calls_the_server() {
    let client = FakeClient::answering(vec![]);
    let (metadata, meta_calls) =
        counting_metadata(|| Ok(Some(meta_map(json!({"trace_id": "abc-123"})))));
    let tool = McpTool::new(weather_tool(), client.clone(), true, None, Some(metadata));

    let got = tool
        .run(tool_ctx(confirmation(true)), args())
        .await
        .unwrap();
    assert_eq!(Value::Object(got), json!({"output": "ok"}));
    assert_eq!(meta_calls.load(Ordering::SeqCst), 1);
    let calls = client.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0].1.meta.as_ref().map(|m| m.0.0.clone()),
        Some(meta_map(json!({"trace_id": "abc-123"})))
    );
}

// R4: the provider overrides the static flag both ways and sees the name and the args.
#[tokio::test]
async fn run_confirmation_provider_overrides_the_static_flag() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let make = |answer: bool| -> ConfirmationProvider {
        let seen = seen.clone();
        Arc::new(move |name: &str, args: &Value| {
            seen.lock().unwrap().push((name.to_string(), args.clone()));
            answer
        })
    };

    // Static true, provider false: the call proceeds.
    let client = FakeClient::answering(vec![]);
    let tool = McpTool::new(
        weather_tool(),
        client.clone(),
        true,
        Some(make(false)),
        None,
    );
    tool.run(tool_ctx(None), args()).await.unwrap();
    assert_eq!(client.calls().len(), 1);

    // Static false, provider true: confirmation is required.
    let client = FakeClient::answering(vec![]);
    let tool = McpTool::new(
        weather_tool(),
        client.clone(),
        false,
        Some(make(true)),
        None,
    );
    let ctx = tool_ctx(None);
    let err = tool.run(ctx.clone(), args()).await.unwrap_err();
    assert!(matches!(err, ToolError::ConfirmationRequired), "{err:?}");
    assert!(client.calls().is_empty());
    assert_eq!(
        ctx.actions().requested_tool_confirmations["fc-1"].hint,
        HINT
    );

    assert_eq!(
        *seen.lock().unwrap(),
        vec![
            ("get_weather".to_string(), args()),
            ("get_weather".to_string(), args()),
        ]
    );
}

// R5: the provider's map reaches `_meta`; None and an empty map send no `_meta`.
#[tokio::test]
async fn run_metadata_provider_meta() {
    type Meta = Option<Map<String, Value>>;
    let cases: Vec<(&str, Meta, Meta)> = vec![
        (
            "provider metadata reaches the server",
            Some(meta_map(json!({"trace_id": "abc-123", "tenant": "acme"}))),
            Some(meta_map(json!({"trace_id": "abc-123", "tenant": "acme"}))),
        ),
        ("provider returning None sends no metadata", None, None),
        (
            "provider returning an empty map sends no metadata",
            Some(Map::new()),
            None,
        ),
        (
            "non-reserved look-alike keys pass",
            Some(meta_map(json!({
                "com.example/tenant": "acme",
                "io.modelcontextprotocolx": "custom",
                "ProgressToken": "tok-1"
            }))),
            Some(meta_map(json!({
                "com.example/tenant": "acme",
                "io.modelcontextprotocolx": "custom",
                "ProgressToken": "tok-1"
            }))),
        ),
    ];
    for (name, answer, want) in cases {
        let client = FakeClient::answering(vec![]);
        let (metadata, calls) = counting_metadata(move || Ok(answer.clone()));
        let tool = McpTool::new(weather_tool(), client.clone(), false, None, Some(metadata));
        tool.run(tool_ctx(None), args()).await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1, "{name}");
        let sent = client.calls()[0].1.meta.as_ref().map(|m| m.0.0.clone());
        assert_eq!(sent, want, "{name}");
    }

    // No provider: no `_meta`.
    let client = FakeClient::answering(vec![]);
    mcp_tool(client.clone())
        .run(tool_ctx(None), args())
        .await
        .unwrap();
    assert_eq!(client.calls()[0].1.meta, None);
}

// R5: the provider runs once per call and gets the live tool context.
#[tokio::test]
async fn run_metadata_provider_per_call_with_context() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let recorder = seen.clone();
    let metadata: MetadataProvider = Arc::new(move |ctx: &Context| {
        let mut seen = recorder.lock().unwrap();
        seen.push((ctx.invocation_id().to_string(), ctx.function_call_id()));
        Ok(Some(meta_map(
            json!({"trace_id": format!("call-{}", seen.len())}),
        )))
    });
    let client = FakeClient::answering(vec![]);
    let tool = McpTool::new(weather_tool(), client.clone(), false, None, Some(metadata));
    tool.run(tool_ctx(None), args()).await.unwrap();
    tool.run(tool_ctx(None), args()).await.unwrap();

    assert_eq!(
        *seen.lock().unwrap(),
        vec![
            ("inv-1".to_string(), "fc-1".to_string()),
            ("inv-1".to_string(), "fc-1".to_string()),
        ]
    );
    let metas: Vec<_> = client
        .calls()
        .iter()
        .map(|(_, p)| p.meta.as_ref().map(|m| m.0.0.clone()))
        .collect();
    assert_eq!(
        metas,
        vec![
            Some(meta_map(json!({"trace_id": "call-1"}))),
            Some(meta_map(json!({"trace_id": "call-2"}))),
        ]
    );
}

// R5: a provider error aborts the call with the tool name and keeps the provider's error as source.
#[tokio::test]
async fn run_metadata_provider_error() {
    let client = FakeClient::answering(vec![]);
    let (metadata, _) = counting_metadata(|| Err("no trace id in context".into()));
    let tool = McpTool::new(weather_tool(), client.clone(), false, None, Some(metadata));
    let err = tool.run(tool_ctx(None), args()).await.unwrap_err();
    assert_eq!(
        err.to_string(),
        r#"metadata provider for MCP tool "get_weather" failed: no trace id in context"#
    );
    let source = execution_source(&err);
    let Error::MetadataProvider { source, .. } = source else {
        panic!("want Error::MetadataProvider, got {source:?}");
    };
    assert_eq!(source.to_string(), "no trace id in context");
    assert!(client.calls().is_empty());
}

// R5: reserved keys are rejected (the first in sorted order is named) and nothing is sent.
#[tokio::test]
async fn run_metadata_provider_rejects_reserved_keys() {
    let cases = [
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
    for (key, val) in cases {
        let client = FakeClient::answering(vec![]);
        let mut map = meta_map(json!({"trace_id": "abc-123"}));
        map.insert(key.to_string(), val);
        let (metadata, _) = counting_metadata(move || Ok(Some(map.clone())));
        let tool = McpTool::new(weather_tool(), client.clone(), false, None, Some(metadata));
        let err = tool.run(tool_ctx(None), args()).await.unwrap_err();
        assert_eq!(
            err.to_string(),
            format!(
                r#"metadata provider for MCP tool "get_weather" returned reserved _meta key "{key}""#
            ),
            "{key}"
        );
        assert!(
            matches!(execution_source(&err), Error::ReservedMetaKey { .. }),
            "{key}"
        );
        assert!(client.calls().is_empty(), "{key}");
    }

    // Sorted order: "io.modelcontextprotocol/…" sorts before "progressToken".
    let client = FakeClient::answering(vec![]);
    let (metadata, _) = counting_metadata(|| {
        Ok(Some(meta_map(json!({
            "progressToken": 1,
            "io.modelcontextprotocol/logLevel": "debug"
        }))))
    });
    let tool = McpTool::new(weather_tool(), client, false, None, Some(metadata));
    let err = tool.run(tool_ctx(None), args()).await.unwrap_err();
    assert_eq!(
        err.to_string(),
        r#"metadata provider for MCP tool "get_weather" returned reserved _meta key "io.modelcontextprotocol/logLevel""#
    );
}
