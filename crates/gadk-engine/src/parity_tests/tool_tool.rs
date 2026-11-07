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

//! Port of Go tool/tool_test.go.

use std::any::Any;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use gadk_agent::tool_util::append_instructions;
use gadk_agent::{
    AgentConfig, ConfirmationProvider, Context, ContextDelta, ReadonlyContext, RunnableTool,
    StreamingTool, Tool, ToolError, Toolset, new_agent, pack_tool, tool_from_request,
    with_confirmation,
};
use gadk_genai::FunctionDeclaration;
use gadk_model::LlmRequest;
use gadk_session::ToolConfirmation;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::tool::{AgentTool, FunctionTool, FunctionToolConfig, StreamingFunctionTool};

const FC_ID: &str = "test-function-call-id";

fn test_context(confirmation: Option<ToolConfirmation>) -> Context {
    Context::builder().build().with_delta(ContextDelta {
        function_call_id: Some(FC_ID.to_string()),
        tool_confirmation: Some(confirmation),
        ..Default::default()
    })
}

/// (name, require, provider, confirmation, want_requested, want_skip, want_err, want_ran)
type ConfirmationCase = (
    &'static str,
    bool,
    Option<ConfirmationProvider>,
    Option<ToolConfirmation>,
    bool,
    bool,
    Option<&'static str>,
    bool,
);

fn confirmation_requested(ctx: &Context) -> bool {
    ctx.actions()
        .requested_tool_confirmations
        .contains_key(FC_ID)
}

struct TestToolset {
    tools: Vec<Arc<dyn Tool>>,
}

#[async_trait]
impl Toolset for TestToolset {
    fn name(&self) -> &str {
        "testToolset"
    }
    async fn tools(&self, _ctx: &ReadonlyContext) -> Result<Vec<Arc<dyn Tool>>, ToolError> {
        Ok(self.tools.clone())
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[derive(Deserialize, Serialize, JsonSchema)]
struct Empty {}

// parity: adapted: geminitool and loadartifactstool are out of scope (spec § Non-Goals); Go's FunctionTool/RequestProcessor interface checks map to as_runnable() and declaration() (packing is generic via pack_tool)
#[tokio::test]
async fn test_types() {
    #[derive(Deserialize, Serialize, JsonSchema)]
    struct IntIo {
        value: i64,
    }
    let function_tool =
        FunctionTool::new(
            FunctionToolConfig::new("", ""),
            |_ctx: Context, input: IntIo| async move {
                Ok::<_, ToolError>(IntIo { value: input.value })
            },
        )
        .unwrap();
    let agent_tool = AgentTool::new(
        new_agent(AgentConfig {
            name: "agent".into(),
            ..Default::default()
        })
        .unwrap(),
        None,
    );
    for (name, tool) in [("FunctionTool", function_tool), ("AgentTool", agent_tool)] {
        assert!(
            tool.as_runnable().is_some(),
            "expected {name} to be runnable (Go toolinternal.FunctionTool)"
        );
        assert!(
            tool.declaration().is_some(),
            "expected {name} to be packable (Go toolinternal.RequestProcessor)"
        );
    }
}

#[tokio::test]
async fn test_with_confirmation() {
    let tool_ran = Arc::new(AtomicBool::new(false));
    let tr = tool_ran.clone();
    let noop = FunctionTool::new(
        FunctionToolConfig::new("noOpTool", ""),
        move |_ctx: Context, _in: Empty| {
            tr.store(true, SeqCst);
            async { Ok::<_, ToolError>(Empty {}) }
        },
    )
    .unwrap();
    let ts: Arc<dyn Toolset> = Arc::new(TestToolset { tools: vec![noop] });
    let yes: ConfirmationProvider = Arc::new(|_name: &str, _input: &Value| true);
    let no: ConfirmationProvider = Arc::new(|_name: &str, _input: &Value| false);
    let confirmed = |c: bool| {
        Some(ToolConfirmation {
            confirmed: c,
            ..Default::default()
        })
    };

    let cases: Vec<ConfirmationCase> = vec![
        (
            "confirmation required, no confirmation in context",
            true,
            None,
            None,
            true,
            true,
            Some("requires confirmation"),
            false,
        ),
        (
            "confirmation required, confirmed in context",
            true,
            None,
            confirmed(true),
            false,
            false,
            None,
            true,
        ),
        (
            "confirmation required, rejected in context",
            true,
            None,
            confirmed(false),
            false,
            false,
            Some("call is rejected"),
            false,
        ),
        (
            "confirmation not required",
            false,
            None,
            None,
            false,
            false,
            None,
            true,
        ),
        (
            "confirmation provider requires confirmation",
            false,
            Some(yes.clone()),
            None,
            true,
            true,
            Some("requires confirmation"),
            false,
        ),
        (
            "confirmation provider does not require confirmation",
            false,
            Some(no.clone()),
            None,
            false,
            false,
            None,
            true,
        ),
        (
            "requireConfirmation=true but provider returns false",
            true,
            Some(no.clone()),
            None,
            false,
            false,
            None,
            true,
        ),
    ];
    for (name, require, provider, confirmation, want_requested, want_skip, want_err, want_ran) in
        cases
    {
        tool_ran.store(false, SeqCst);
        let ctx = test_context(confirmation);
        let cts = with_confirmation(ts.clone(), require, provider);
        let tools = cts.tools(ctx.as_readonly()).await.unwrap();
        assert_eq!(tools.len(), 1, "{name}");
        let runnable = tools[0]
            .as_runnable()
            .unwrap_or_else(|| panic!("{name}: tools[0] is not runnable"));
        let res = runnable.run(ctx.clone(), json!({})).await;
        match want_err {
            Some(msg) => {
                let e = res.expect_err(name);
                assert!(
                    e.to_string().contains(msg),
                    "{name}: Run() error msg = {e}, want it to contain {msg:?}"
                );
            }
            None => assert!(res.is_ok(), "{name}: Run() error = {:?}", res.err()),
        }
        assert_eq!(
            ctx.actions().skip_summarization,
            want_skip,
            "{name}: skip_summarization"
        );
        assert_eq!(
            confirmation_requested(&ctx),
            want_requested,
            "{name}: confirmation requested"
        );
        assert_eq!(tool_ran.load(SeqCst), want_ran, "{name}: tool ran");
    }
}

/// Go `mockCustomRequestTool` / `mockCustomRequestPackingTool`: name and description come from the
/// embedded function tool; the declaration is overridden; `process_request` records the call and,
/// for the request tool, appends a system instruction.
struct CustomRequestTool {
    base: Arc<dyn Tool>,
    decl_name: &'static str,
    append_instruction: bool,
    called: Arc<AtomicBool>,
}

#[async_trait]
impl Tool for CustomRequestTool {
    fn name(&self) -> &str {
        self.base.name()
    }
    fn description(&self) -> &str {
        self.base.description()
    }
    fn declaration(&self) -> Option<FunctionDeclaration> {
        Some(FunctionDeclaration {
            name: self.decl_name.into(),
            ..Default::default()
        })
    }
    async fn process_request(&self, _ctx: &Context, req: &mut LlmRequest) -> Result<(), ToolError> {
        self.called.store(true, SeqCst);
        if self.append_instruction {
            append_instructions(req, &["custom tool system instruction"]);
        }
        Ok(())
    }
    fn as_runnable(&self) -> Option<&dyn RunnableTool> {
        Some(self)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[async_trait]
impl RunnableTool for CustomRequestTool {
    async fn run(&self, _ctx: Context, _args: Value) -> Result<Map<String, Value>, ToolError> {
        Ok(Map::new())
    }
}

fn custom_request_tool(
    name: &'static str,
    append_instruction: bool,
    called: &Arc<AtomicBool>,
) -> Arc<dyn Tool> {
    let base = FunctionTool::new(
        FunctionToolConfig::new(name, ""),
        |_ctx: Context, _in: Empty| async { Ok::<_, ToolError>(Empty {}) },
    )
    .unwrap_or_else(|e| panic!("FunctionTool::new() failed: {e}"));
    Arc::new(CustomRequestTool {
        base,
        decl_name: name,
        append_instruction,
        called: called.clone(),
    })
}

async fn wrapped_tools(inner: Arc<dyn Tool>) -> Vec<Arc<dyn Tool>> {
    let ts: Arc<dyn Toolset> = Arc::new(TestToolset { tools: vec![inner] });
    let tools = with_confirmation(ts, true, None)
        .tools(Context::builder().build().as_readonly())
        .await
        .unwrap_or_else(|e| panic!("cts.Tools() failed: {e}"));
    assert_eq!(
        tools.len(),
        1,
        "cts.Tools() returned {} tools, want 1",
        tools.len()
    );
    tools
}

// parity: adapted: Rust Tool::process_request does extra edits only (packing is pack_tool), so the port asserts the wrapper forwards process_request to the inner tool
#[tokio::test]
async fn test_with_confirmation_process_request() {
    let called = Arc::new(AtomicBool::new(false));
    let tools = wrapped_tools(custom_request_tool("customReqTool", true, &called)).await;

    let mut req = LlmRequest::default();
    pack_tool(&mut req, &tools[0]).unwrap_or_else(|e| panic!("pack_tool() failed: {e}"));
    tools[0]
        .process_request(&test_context(None), &mut req)
        .await
        .unwrap_or_else(|e| panic!("ProcessRequest() failed: {e}"));

    assert!(
        called.load(SeqCst),
        "expected wrapped tool's ProcessRequest to be called, but it was not"
    );
    let si = req
        .config
        .as_ref()
        .and_then(|c| c.system_instruction.as_ref())
        .filter(|si| !si.parts.is_empty())
        .expect("expected SystemInstruction to be set on req.Config, got nil/empty");
    assert_eq!(
        si.parts[0].text, "custom tool system instruction",
        "SystemInstruction text"
    );
}

// parity: adapted: Go's inner tool packs itself from ProcessRequest; in Rust the flow packs with pack_tool, so the port asserts the packed entry is the gated wrapper
#[tokio::test]
async fn test_with_confirmation_process_request_packing() {
    let called = Arc::new(AtomicBool::new(false));
    let tools = wrapped_tools(custom_request_tool("customPackingTool", false, &called)).await;

    let mut req = LlmRequest::default();
    pack_tool(&mut req, &tools[0]).unwrap_or_else(|e| panic!("pack_tool() failed: {e}"));
    tools[0]
        .process_request(&test_context(None), &mut req)
        .await
        .unwrap_or_else(|e| panic!("ProcessRequest() failed: {e}"));

    assert!(
        called.load(SeqCst),
        "expected wrapped tool's ProcessRequest to be called, but it was not"
    );
    let packed =
        tool_from_request(&req, "customPackingTool").expect("req.tools has no customPackingTool");
    assert!(
        Arc::ptr_eq(&packed, &tools[0]),
        "expected req.tools[\"customPackingTool\"] to be the confirmation wrapper"
    );
    let runnable = packed
        .as_runnable()
        .expect("expected packed tool to be runnable");

    let ctx = test_context(None);
    let res = runnable.run(ctx.clone(), json!({})).await;
    assert!(
        matches!(res, Err(ToolError::ConfirmationRequired)),
        "expected Run() to return ErrConfirmationRequired, got {res:?}"
    );
    assert!(
        confirmation_requested(&ctx),
        "expected RequestConfirmation to be called on ctx, but it was not"
    );
}

#[tokio::test]
async fn test_with_confirmation_streaming_tool() {
    let yes: ConfirmationProvider = Arc::new(|_name: &str, _input: &Value| true);
    let no: ConfirmationProvider = Arc::new(|_name: &str, _input: &Value| false);
    let confirmed = |c: bool| {
        Some(ToolConfirmation {
            confirmed: c,
            ..Default::default()
        })
    };

    let cases: Vec<ConfirmationCase> = vec![
        (
            "confirmation required, no confirmation in context",
            true,
            None,
            None,
            true,
            true,
            Some("requires confirmation"),
            false,
        ),
        (
            "confirmation required, confirmed in context",
            true,
            None,
            confirmed(true),
            false,
            false,
            None,
            true,
        ),
        (
            "confirmation required, rejected in context",
            true,
            None,
            confirmed(false),
            false,
            false,
            Some("call is rejected"),
            false,
        ),
        (
            "confirmation not required",
            false,
            None,
            None,
            false,
            false,
            None,
            true,
        ),
        (
            "provider requires confirmation",
            false,
            Some(yes),
            None,
            true,
            true,
            Some("requires confirmation"),
            false,
        ),
        (
            "requireConfirmation=true but provider returns false",
            true,
            Some(no),
            None,
            false,
            false,
            None,
            true,
        ),
    ];
    for (name, require, provider, confirmation, want_requested, want_skip, want_err, want_ran) in
        cases
    {
        let handler_ran = Arc::new(AtomicBool::new(false));
        let ran = handler_ran.clone();
        // The tool itself asks for no confirmation. Only the toolset does, which is the case the
        // wrapper exists to serve.
        let stream_tool = StreamingFunctionTool::new(
            FunctionToolConfig::new("streamTool", ""),
            move |_ctx: Context, _in: Empty| {
                let ran = ran.clone();
                futures::stream::once(async move {
                    ran.store(true, SeqCst);
                    Ok::<_, ToolError>("did the thing".to_string())
                })
            },
        )
        .unwrap_or_else(|e| panic!("{name}: StreamingFunctionTool::new() failed: {e}"));

        let ts: Arc<dyn Toolset> = Arc::new(TestToolset {
            tools: vec![stream_tool],
        });
        let ctx = test_context(confirmation);
        let tools = with_confirmation(ts, require, provider)
            .tools(ctx.as_readonly())
            .await
            .unwrap_or_else(|e| panic!("{name}: cts.Tools() failed: {e}"));
        assert_eq!(
            tools.len(),
            1,
            "{name}: cts.Tools() returned {} tools, want 1",
            tools.len()
        );
        let wrapped = tools[0].as_streaming().unwrap_or_else(|| {
            panic!("{name}: tools[0] is not a StreamingFunctionTool, the flow could not execute it")
        });

        let mut got_err = None;
        let mut s = wrapped.run_stream(ctx.clone(), json!({}));
        while let Some(item) = s.next().await {
            if let Err(e) = item {
                got_err = Some(e);
            }
        }

        match want_err {
            None => assert!(
                got_err.is_none(),
                "{name}: RunStream() error = {got_err:?}, want none"
            ),
            Some(msg) => {
                let e = got_err.unwrap_or_else(|| {
                    panic!("{name}: RunStream() error = nil, want it to contain {msg:?}")
                });
                assert!(
                    e.to_string().contains(msg),
                    "{name}: RunStream() error msg = {e}, want it to contain {msg:?}"
                );
            }
        }
        assert_eq!(handler_ran.load(SeqCst), want_ran, "{name}: handlerRan");
        assert_eq!(
            confirmation_requested(&ctx),
            want_requested,
            "{name}: requestConfirmationCalled"
        );
        assert_eq!(
            ctx.actions().skip_summarization,
            want_skip,
            "{name}: SkipSummarization"
        );
    }
}

/// Go `dualTool`: both the runnable and the streaming shapes. The flow dispatches on the streaming
/// shape first, so the confirmation wrapper has to do the same.
struct DualTool {
    stream_ran: Arc<AtomicBool>,
    run_ran: Arc<AtomicBool>,
}

impl Tool for DualTool {
    fn name(&self) -> &str {
        "dualTool"
    }
    fn description(&self) -> &str {
        ""
    }
    fn declaration(&self) -> Option<FunctionDeclaration> {
        Some(FunctionDeclaration {
            name: self.name().into(),
            ..Default::default()
        })
    }
    fn as_runnable(&self) -> Option<&dyn RunnableTool> {
        Some(self)
    }
    fn as_streaming(&self) -> Option<&dyn StreamingTool> {
        Some(self)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[async_trait]
impl RunnableTool for DualTool {
    async fn run(&self, _ctx: Context, _args: Value) -> Result<Map<String, Value>, ToolError> {
        self.run_ran.store(true, SeqCst);
        Ok(Map::new())
    }
}

impl StreamingTool for DualTool {
    fn run_stream(
        &self,
        _ctx: Context,
        _args: Value,
    ) -> BoxStream<'static, Result<String, ToolError>> {
        let ran = self.stream_ran.clone();
        futures::stream::once(async move {
            ran.store(true, SeqCst);
            Ok("chunk".to_string())
        })
        .boxed()
    }
}

#[tokio::test]
async fn test_with_confirmation_tool_with_both_run_and_run_stream() {
    let stream_ran = Arc::new(AtomicBool::new(false));
    let run_ran = Arc::new(AtomicBool::new(false));
    let dt: Arc<dyn Tool> = Arc::new(DualTool {
        stream_ran: stream_ran.clone(),
        run_ran: run_ran.clone(),
    });
    let tools = wrapped_tools(dt).await;

    // The flow checks the streaming shape before the runnable one, so the wrapper must present
    // the streaming shape and must not leak an unguarded Run.
    let streaming = tools[0]
        .as_streaming()
        .expect("wrapped tool is not a StreamingFunctionTool");
    assert!(
        tools[0].as_runnable().is_none(),
        "wrapped tool also satisfies FunctionTool, so an unguarded Run is reachable"
    );

    let ctx = test_context(None);
    let mut s = streaming.run_stream(ctx.clone(), json!({}));
    while let Some(item) = s.next().await {
        assert!(
            matches!(item, Err(ToolError::ConfirmationRequired)),
            "RunStream() item = {item:?}, want ErrConfirmationRequired"
        );
    }
    assert!(
        !stream_ran.load(SeqCst) && !run_ran.load(SeqCst),
        "tool executed without confirmation: streamRan={} runRan={}",
        stream_ran.load(SeqCst),
        run_ran.load(SeqCst)
    );
    assert!(
        confirmation_requested(&ctx),
        "requestConfirmationCalled = false, want true"
    );
}

const STREAM_PACKING_TOOL: &str = "streamPackingTool";

/// Go `bareStreamingTool` (no `process_request` of its own) and, with `called` set, Go
/// `selfPackingStreamingTool` (its `process_request` records the call; packing is `pack_tool`'s).
struct PackingStreamingTool {
    stream_ran: Arc<AtomicBool>,
    called: Option<Arc<AtomicBool>>,
}

#[async_trait]
impl Tool for PackingStreamingTool {
    fn name(&self) -> &str {
        STREAM_PACKING_TOOL
    }
    fn description(&self) -> &str {
        ""
    }
    fn declaration(&self) -> Option<FunctionDeclaration> {
        Some(FunctionDeclaration {
            name: STREAM_PACKING_TOOL.into(),
            ..Default::default()
        })
    }
    async fn process_request(
        &self,
        _ctx: &Context,
        _req: &mut LlmRequest,
    ) -> Result<(), ToolError> {
        if let Some(called) = &self.called {
            called.store(true, SeqCst);
        }
        Ok(())
    }
    fn as_streaming(&self) -> Option<&dyn StreamingTool> {
        Some(self)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl StreamingTool for PackingStreamingTool {
    fn run_stream(
        &self,
        _ctx: Context,
        _args: Value,
    ) -> BoxStream<'static, Result<String, ToolError>> {
        let ran = self.stream_ran.clone();
        futures::stream::once(async move {
            ran.store(true, SeqCst);
            Ok("did the thing".to_string())
        })
        .boxed()
    }
}

// parity: adapted: packing is pack_tool in Rust; the inner tool's process_request is still forwarded
#[tokio::test]
async fn test_with_confirmation_streaming_process_request_packing() {
    for (name, self_packing, want_called) in [
        ("inner tool packs itself", true, true),
        ("inner tool has no ProcessRequest", false, false),
    ] {
        let stream_ran = Arc::new(AtomicBool::new(false));
        let called = Arc::new(AtomicBool::new(false));
        let inner: Arc<dyn Tool> = Arc::new(PackingStreamingTool {
            stream_ran: stream_ran.clone(),
            called: self_packing.then(|| called.clone()),
        });
        let tools = wrapped_tools(inner).await;

        let mut req = LlmRequest::default();
        pack_tool(&mut req, &tools[0])
            .unwrap_or_else(|e| panic!("{name}: pack_tool() failed: {e}"));
        tools[0]
            .process_request(&test_context(None), &mut req)
            .await
            .unwrap_or_else(|e| panic!("{name}: ProcessRequest() failed: {e}"));
        assert_eq!(
            called.load(SeqCst),
            want_called,
            "{name}: inner ProcessRequest called"
        );

        // The flow executes whatever req.tools holds under this name, so an inner tool left there
        // is an ungated tool.
        let packed = tool_from_request(&req, STREAM_PACKING_TOOL)
            .unwrap_or_else(|| panic!("{name}: req.tools has no {STREAM_PACKING_TOOL}"));
        assert!(
            Arc::ptr_eq(&packed, &tools[0]),
            "{name}: req.tools[{STREAM_PACKING_TOOL:?}] is not the confirmation wrapper"
        );
        let streaming = packed
            .as_streaming()
            .unwrap_or_else(|| panic!("{name}: packed tool is not a StreamingFunctionTool, so the flow could not execute it"));
        assert!(
            packed.as_runnable().is_none(),
            "{name}: packed tool also satisfies FunctionTool, so an unguarded Run is reachable"
        );

        let ctx = test_context(None);
        let mut s = streaming.run_stream(ctx.clone(), json!({}));
        while let Some(item) = s.next().await {
            assert!(
                matches!(item, Err(ToolError::ConfirmationRequired)),
                "{name}: RunStream() item = {item:?}, want ErrConfirmationRequired"
            );
        }
        assert!(
            !stream_ran.load(SeqCst),
            "{name}: packed tool ran without confirmation"
        );
        assert!(
            confirmation_requested(&ctx),
            "{name}: requestConfirmationCalled = false, want true"
        );
    }
}
