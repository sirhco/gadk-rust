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

//! Tools request processor (port of Go internal/llminternal/tools_processor.go) and the flow's
//! tool/toolset preprocessing (Go base_flow.go toolPreprocess, toolsetPreprocess).

use std::sync::Arc;

use futures::future::BoxFuture;
use gadk_agent::{AgentError, ContextDelta, InvocationContext, Tool, pack_tool};
use gadk_model::LlmRequest;
use gadk_session::{Event, EventActions};

use crate::{
    llm::{flow::Flow, processors::wrap_err, state::reveal},
    workflow::route::go_quote,
};

/// Port of tools_processor.go:32 toolProcessor: rebuilds Flow.tools on every step (toolsets are
/// re-evaluated each step).
pub(crate) fn tools_processor<'a>(
    ctx: &'a InvocationContext,
    _req: &'a mut LlmRequest,
    f: &'a Flow,
) -> BoxFuture<'a, Result<Vec<Event>, AgentError>> {
    Box::pin(async move {
        let agent = ctx.agent();
        let Some(state) = reveal(&*agent) else {
            return Err(AgentError::other(format!(
                "agent {} is not an LLMAgent",
                agent.name()
            )));
        };
        let mut tools: Vec<Arc<dyn Tool>> = state.tools();
        let ro = ctx.as_readonly().clone();
        for ts in &state.cfg.toolsets {
            let ts_tools = ts.tools(&ro).await.map_err(|e| {
                wrap_err(
                    format!(
                        "failed to extract tools from the tool set {}",
                        go_quote(ts.name())
                    ),
                    e,
                )
            })?;
            tools.extend(ts_tools);
        }
        *f.tools.write() = tools;
        Ok(Vec::new())
    })
}

/// Port of base_flow.go:951 toolPreprocess. Runs AFTER all request processors (called by Task
/// 11's Flow::preprocess). For each tool, in order: pack_tool (Go toolutils.PackTool, which Go
/// tools call inside ProcessRequest), then the tool's own process_request for extra edits (e.g.
/// finish_task's appended instructions). This keeps Go's declaration order: processor-added
/// tools (transfer_to_agent, set_model_response) come first, then the agent's tools.
///
/// Go fails with "does not implement RequestProcessor()" for a tool without `ProcessRequest`;
/// every Rust `Tool` has a defaulted `process_request`, so that error cannot occur.
pub(crate) fn tool_preprocess(
    ctx: &InvocationContext,
    req: &mut LlmRequest,
    tools: &[Arc<dyn Tool>],
) -> Result<(), AgentError> {
    for t in tools {
        // Go: agent.NewToolContext(ctx, "", &session.EventActions{}, nil).
        let tool_ctx = ctx.clone().into_context().with_delta(ContextDelta {
            function_call_id: Some(String::new()),
            actions: Some(Arc::new(parking_lot::Mutex::new(EventActions::default()))),
            tool_confirmation: Some(None),
            ..Default::default()
        });
        // Text `duplicate tool: "x"` unchanged.
        pack_tool(req, t).map_err(|e| AgentError::Other(Box::new(e)))?;
        t.process_request(&tool_ctx, req)
            .map_err(|e| AgentError::Other(Box::new(e)))?;
    }
    Ok(())
}

/// Port of base_flow.go:966 toolsetPreprocess.
pub(crate) async fn toolset_preprocess(
    ctx: &InvocationContext,
    req: &mut LlmRequest,
) -> Result<(), AgentError> {
    let agent = ctx.agent();
    let Some(state) = reveal(&*agent) else {
        return Ok(());
    };
    for ts in &state.cfg.toolsets {
        // Go: agent.NewToolContext(ctx, "", nil, nil): fresh actions.
        let tool_ctx = ctx.clone().into_context().with_delta(ContextDelta {
            function_call_id: Some(String::new()),
            tool_confirmation: Some(None),
            ..Default::default()
        });
        ts.process_request(&tool_ctx, req).await.map_err(|e| {
            wrap_err(
                format!("process request by toolset {}", go_quote(ts.name())),
                e,
            )
        })?;
    }
    Ok(())
}
