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

#![cfg(feature = "mcp")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Port of Go tool/mcptoolset/prefix_test.go (`Config::prefix`, #1821). The toolsets run over
//! in-process rmcp servers (`gadk::mcp::testutil`); `TestMCPToolNamePrefixRunner` drives an LLM
//! agent through the runner with a scripted model.

use std::sync::{Arc, Mutex};

use futures::StreamExt;
use gadk::agent::tool::{ConfirmationProvider, Tool, Toolset};
use gadk::agent::tool_util::{pack_tool, tool_from_request};
use gadk::agent::{
    BeforeToolCallback, Context, ContextDelta, ToolError, allowed_tools_predicate, filter_toolset,
    with_confirmation,
};
use gadk::engine::llm::{LlmAgentConfig, agent as llm_agent};
use gadk::engine::testutil::{MockModel, TestAgentRunner};
use gadk::genai::{Content, FunctionCall, Part, ROLE_MODEL};
use gadk::mcp::rmcp::model::{
    CallToolRequestParams, CallToolResult, ContentBlock, Tool as McpToolDef,
};
use gadk::mcp::testutil::{ToolServer, in_memory};
use gadk::mcp::{Config, McpToolset, McpTransport};
use gadk::model::LlmRequest;
use gadk::session::ToolConfirmation;
use serde_json::{Map, Value, json};

/// The server-side names `tools/call` received.
type Calls = Arc<Mutex<Vec<String>>>;

/// Go `newPrefixTestToolset`: a `search_server` with one tool per name (input schema
/// `{"type": "object"}`), each recording the called name and answering the text `cfg.prefix`.
fn new_prefix_test_toolset(mut cfg: Config, names: &[&str]) -> (Arc<McpToolset>, Calls) {
    let calls: Calls = Arc::default();
    let mut server = ToolServer::new("search_server", "1");
    for name in names {
        let tool = McpToolDef::new_with_raw(
            (*name).to_string(),
            Some("Search a collection".into()),
            Arc::new(Map::from_iter([("type".to_string(), json!("object"))])),
        );
        let calls = calls.clone();
        let answer = cfg.prefix.clone();
        server = server.tool(tool, move |params: CallToolRequestParams, _| {
            calls.lock().unwrap().push(params.name.to_string());
            let answer = answer.clone();
            async move { Ok(CallToolResult::success(vec![ContentBlock::text(answer)])) }
        });
    }
    cfg.transport = McpTransport::Custom(in_memory(move || server.clone()));
    let ts = gadk::mcp::new(cfg).unwrap_or_else(|e| panic!("mcptoolset.New() failed: {e}"));
    (Arc::new(ts), calls)
}

/// Go `mcptoolset.Config{ToolNamePrefix: prefix}` (the transport is set by
/// [`new_prefix_test_toolset`]).
fn prefix_config(prefix: &str) -> Config {
    let mut cfg = Config::new(McpTransport::Endpoint(String::new()));
    cfg.prefix = prefix.to_string();
    cfg
}

/// Go `prefixTestTools`.
async fn prefix_test_tools(ts: &dyn Toolset) -> Vec<Arc<dyn Tool>> {
    ts.tools(&Context::builder().build())
        .await
        .unwrap_or_else(|e| panic!("Tools() failed: {e}"))
}

fn calls_of(calls: &Calls) -> Vec<String> {
    calls.lock().unwrap().clone()
}

#[tokio::test]
async fn test_mcp_tool_name_prefix_runner() {
    let (docs, docs_calls) = new_prefix_test_toolset(prefix_config("docs"), &["search"]);
    let (issues, issues_calls) = new_prefix_test_toolset(prefix_config("issues"), &["search"]);
    let call = |id: &str, name: &str| Part {
        function_call: Some(FunctionCall {
            id: id.into(),
            name: name.into(),
            args: Map::new(),
            ..Default::default()
        }),
        ..Default::default()
    };
    let llm = MockModel::from_contents(vec![
        Content {
            role: ROLE_MODEL.into(),
            parts: vec![
                call("docs_call", "docs_search"),
                call("issues_call", "issues_search"),
            ],
        },
        Content::from_text("done", ROLE_MODEL),
    ]);
    let callback_names: Arc<Mutex<Vec<String>>> = Arc::default();
    let names = callback_names.clone();
    let before_tool: BeforeToolCallback = Arc::new(move |_ctx, called, args| {
        names.lock().unwrap().push(called.name().to_string());
        Box::pin(async move { (args, Ok(None)) })
    });
    let agent = llm_agent::new(LlmAgentConfig {
        name: "search_agent".into(),
        model: Some(llm),
        toolsets: vec![docs.clone(), issues.clone()],
        before_tool_callbacks: vec![before_tool],
        ..Default::default()
    })
    .unwrap_or_else(|e| panic!("llmagent.New() failed: {e}"));

    let mut want_responses: Map<String, Value> = Map::from_iter([
        ("docs_search".into(), json!("docs")),
        ("issues_search".into(), json!("issues")),
    ]);
    let mut responses = 0;
    let runner = TestAgentRunner::new(agent).await;
    let mut events = runner.run("Search both collections");
    while let Some(event) = events.next().await {
        let event = event.unwrap_or_else(|e| panic!("Runner.Run() failed: {e}"));
        let Some(content) = &event.response.content else {
            continue;
        };
        for part in &content.parts {
            if let Some(response) = &part.function_response {
                let source = want_responses.get(&response.name).cloned();
                let matches = source.as_ref().is_some_and(|source| {
                    response.response.get("output") == Some(source)
                        && Some(response.id.as_str())
                            == source.as_str().map(|s| format!("{s}_call")).as_deref()
                });
                assert!(
                    matches,
                    "function response does not match its tool, server, and call ID: {response:?}"
                );
                want_responses.remove(&response.name);
                responses += 1;
            }
        }
    }
    assert!(
        responses == 2 && want_responses.is_empty(),
        "received {responses} function responses, want one per server"
    );
    for calls in [&docs_calls, &issues_calls] {
        assert_eq!(
            calls_of(calls),
            vec!["search".to_string()],
            "MCP server received a renamed tool, or not exactly one call"
        );
    }
    let mut names = callback_names.lock().unwrap().clone();
    names.sort();
    assert_eq!(
        names,
        vec!["docs_search", "issues_search"],
        "tool callbacks did not receive exposed names"
    );
    docs.close().await.unwrap();
    issues.close().await.unwrap();
}

// parity: adapted: Go's ProcessRequest is toolutils.PackTool, which gadk's flow runs for every declared tool; the port packs the tool with gadk_agent::tool_util::pack_tool and reads it back with tool_from_request
#[tokio::test]
async fn test_mcp_tool_name_prefix_registration() {
    for (name, prefix, want) in [
        ("empty", "", "search"),
        ("prefix", "docs", "docs_search"),
        ("literal prefix", "docs_", "docs__search"),
    ] {
        let (ts, _) = new_prefix_test_toolset(prefix_config(prefix), &["search"]);
        let first = prefix_test_tools(ts.as_ref()).await;
        let second = prefix_test_tools(ts.as_ref()).await;
        for tools in [&first, &second] {
            assert_eq!(tools.len(), 1, "{name}: Tools() returned tools");
            let decl = tools[0].declaration().unwrap();
            assert!(
                tools[0].name() == want && decl.name == want,
                "{name}: tool and declaration names do not match the configured prefix"
            );
        }
        let mut req = LlmRequest::default();
        pack_tool(&mut req, &first[0]).unwrap_or_else(|e| panic!("ProcessRequest() failed: {e}"));
        assert!(
            req.tools.len() == 1
                && tool_from_request(&req, want).is_some_and(|t| Arc::ptr_eq(&t, &first[0])),
            "{name}: request registry did not preserve the exposed name and tool"
        );
        let config = req
            .config
            .as_ref()
            .expect("request must contain exactly one function declaration");
        assert!(
            config.tools.len() == 1 && config.tools[0].function_declarations.len() == 1,
            "{name}: request must contain exactly one function declaration"
        );
        assert_eq!(
            config.tools[0].function_declarations[0].name, want,
            "{name}: request declaration does not use the exposed name"
        );
        ts.close().await.unwrap();
    }
}

#[tokio::test]
async fn test_mcp_tool_name_prefix_filter() {
    for (name, internal, external, want) in [
        (
            "original inside, exposed outside",
            "search",
            "docs_search",
            1,
        ),
        (
            "exposed inside is excluded",
            "docs_search",
            "docs_search",
            0,
        ),
        ("original outside is excluded", "search", "search", 0),
    ] {
        let mut cfg = prefix_config("docs");
        cfg.tool_filter = Some(allowed_tools_predicate(&[internal]));
        let (ts, _) = new_prefix_test_toolset(cfg, &["search", "list"]);
        let filtered = filter_toolset(ts.clone(), allowed_tools_predicate(&[external]));
        assert_eq!(
            prefix_test_tools(filtered.as_ref()).await.len(),
            want,
            "{name}: filtered tool count"
        );
        ts.close().await.unwrap();
    }
}

/// Go `agent.NewToolContext(invCtx, "call", nil, confirmation)`.
fn tool_ctx(confirmation: Option<ToolConfirmation>) -> Context {
    Context::builder().build().with_delta(ContextDelta {
        function_call_id: Some("call".into()),
        tool_confirmation: Some(confirmation),
        ..Default::default()
    })
}

// parity: adapted: Go's ProcessRequest is toolutils.PackTool (see the registration test); Go's errors.Is checks are matches on ToolError::ConfirmationRequired / ConfirmationRejected
#[tokio::test]
async fn test_mcp_tool_name_prefix_confirmation() {
    for mode in ["native", "wrapper"] {
        let provider_calls = Arc::new(Mutex::new(0usize));
        let seen = provider_calls.clone();
        let provider: ConfirmationProvider = Arc::new(move |name: &str, _args: &Value| {
            *seen.lock().unwrap() += 1;
            assert_eq!(
                name, "docs_search",
                "confirmation provider did not receive the exposed name"
            );
            true
        });
        let mut cfg = prefix_config("docs");
        if mode == "native" {
            cfg.require_confirmation_provider = Some(provider.clone());
        }
        let (mcp_ts, calls) = new_prefix_test_toolset(cfg, &["search"]);
        let ts: Arc<dyn Toolset> = if mode == "wrapper" {
            with_confirmation(mcp_ts.clone(), false, Some(provider))
        } else {
            mcp_ts.clone()
        };
        for (name, confirmation, want_calls) in [
            ("pending", None, 0),
            (
                "approved",
                Some(ToolConfirmation {
                    confirmed: true,
                    ..Default::default()
                }),
                1,
            ),
            ("rejected", Some(ToolConfirmation::default()), 0),
        ] {
            let case = format!("{mode}/{name}");
            let tools = prefix_test_tools(ts.as_ref()).await;
            assert_eq!(tools.len(), 1, "{case}: Tools() returned tools");
            let mut req = LlmRequest::default();
            pack_tool(&mut req, &tools[0])
                .unwrap_or_else(|e| panic!("{case}: ProcessRequest() failed: {e}"));
            let dispatched = tool_from_request(&req, "docs_search")
                .filter(|t| Arc::ptr_eq(t, &tools[0]))
                .unwrap_or_else(|| {
                    panic!(
                        "{case}: request registry did not preserve the tool's confirmation wrapper"
                    )
                });
            let pending = confirmation.is_none();
            let ctx = tool_ctx(confirmation);
            let result = dispatched
                .as_runnable()
                .expect("tool is not runnable")
                .run(ctx.clone(), json!({}))
                .await;
            match name {
                "pending" => assert!(
                    matches!(result, Err(ToolError::ConfirmationRequired)),
                    "{case}: Run() = {result:?}, want ConfirmationRequired"
                ),
                "approved" => assert!(result.is_ok(), "{case}: Run() = {result:?}, want Ok"),
                _ => assert!(
                    matches!(result, Err(ToolError::ConfirmationRejected)),
                    "{case}: Run() = {result:?}, want ConfirmationRejected"
                ),
            }
            if pending {
                let actions = ctx.actions();
                let ok = actions
                    .requested_tool_confirmations
                    .get("call")
                    .is_some_and(|c| c.hint.contains("docs_search()"))
                    && actions.skip_summarization;
                assert!(
                    ok,
                    "{case}: pending confirmation does not identify the exposed tool or pause the flow"
                );
            }
            // Go reads (drains) the approved call from its channel.
            let got_calls = std::mem::take(&mut *calls.lock().unwrap());
            assert_eq!(got_calls.len(), want_calls, "{case}: MCP call count");
            if name == "approved" {
                let output = result.as_ref().ok().and_then(|r| r.get("output"));
                assert!(
                    got_calls[0] == "search" && output == Some(&json!("docs")),
                    "{case}: approved call did not execute the original server tool"
                );
            }
        }
        assert_eq!(
            *provider_calls.lock().unwrap(),
            1,
            "{mode}: confirmation provider call count"
        );
        mcp_ts.close().await.unwrap();
    }
}
