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

//! `filter_toolset`, `allowed_tools_predicate`, `with_confirmation` (port of Go `tool/tool.go`
//! helpers).

use std::any::Any;
use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use gadk_genai::FunctionDeclaration;
use gadk_model::LlmRequest;
use serde_json::{Map, Value};

use crate::context::{Context, ReadonlyContext};
use crate::error::ToolError;
use crate::tool::{
    ConfirmationProvider, RunnableTool, StreamingTool, Tool, ToolPredicate, Toolset,
};

/// Go `AllowedToolsPredicate`: a predicate that allows only the named tools.
pub fn allowed_tools_predicate(allowed: &[&str]) -> ToolPredicate {
    let set: HashSet<String> = allowed.iter().map(|s| (*s).to_string()).collect();
    Arc::new(move |_ctx, tool| set.contains(tool.name()))
}

/// Go `StringPredicate` (an alias of [`allowed_tools_predicate`]).
pub fn string_predicate(allowed: &[&str]) -> ToolPredicate {
    allowed_tools_predicate(allowed)
}

/// Go `FilterToolset`: a toolset exposing only the inner tools the predicate allows. Like Go's
/// `filteredToolset`, the wrapper exposes only `Name` and `Tools`: the inner toolset's
/// `process_request` is NOT forwarded (default no-op).
pub fn filter_toolset(toolset: Arc<dyn Toolset>, predicate: ToolPredicate) -> Arc<dyn Toolset> {
    Arc::new(FilteredToolset { toolset, predicate })
}

/// Go `filteredToolset`.
struct FilteredToolset {
    toolset: Arc<dyn Toolset>,
    predicate: ToolPredicate,
}

#[async_trait]
impl Toolset for FilteredToolset {
    fn name(&self) -> &str {
        self.toolset.name()
    }

    async fn tools(&self, ctx: &ReadonlyContext) -> Result<Vec<Arc<dyn Tool>>, ToolError> {
        let tools = self.toolset.tools(ctx).await?;
        Ok(tools
            .into_iter()
            .filter(|t| (self.predicate)(ctx, t.as_ref()))
            .collect())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Go `WithConfirmation`. Wraps each tool: streaming-capable → streaming-only wrapper; else
/// runnable → runnable wrapper; else returned unchanged (same `Arc`). When `provider` is set it
/// decides per call and overrides `require_confirmation`. Like Go, the toolset wrapper does not
/// forward `process_request`.
pub fn with_confirmation(
    toolset: Arc<dyn Toolset>,
    require_confirmation: bool,
    provider: Option<ConfirmationProvider>,
) -> Arc<dyn Toolset> {
    Arc::new(ConfirmationToolset {
        toolset,
        require_confirmation,
        provider,
    })
}

/// Go `confirmationToolset`.
struct ConfirmationToolset {
    toolset: Arc<dyn Toolset>,
    require_confirmation: bool,
    provider: Option<ConfirmationProvider>,
}

#[async_trait]
impl Toolset for ConfirmationToolset {
    fn name(&self) -> &str {
        self.toolset.name()
    }

    async fn tools(&self, ctx: &ReadonlyContext) -> Result<Vec<Arc<dyn Tool>>, ToolError> {
        let tools = self.toolset.tools(ctx).await?;
        Ok(tools
            .into_iter()
            .map(|t| {
                let streaming = if t.as_streaming().is_some() {
                    true
                } else if t.as_runnable().is_some() {
                    false
                } else {
                    return t;
                };
                Arc::new(ConfirmationTool {
                    inner: t,
                    require_confirmation: self.require_confirmation,
                    provider: self.provider.clone(),
                    streaming,
                }) as Arc<dyn Tool>
            })
            .collect())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Go `confirmationTool` / `confirmationStreamingTool`. Go embeds the runnable (or streaming)
/// interface, which hides every other optional capability, so `defers_response` /
/// `display_result_on_skip_summarization` / `wrapper_key` keep their defaults, and a
/// streaming-wrapped tool is not runnable.
struct ConfirmationTool {
    inner: Arc<dyn Tool>,
    require_confirmation: bool,
    provider: Option<ConfirmationProvider>,
    streaming: bool,
}

impl Tool for ConfirmationTool {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn description(&self) -> &str {
        self.inner.description()
    }

    fn is_long_running(&self) -> bool {
        self.inner.is_long_running()
    }

    fn declaration(&self) -> Option<FunctionDeclaration> {
        self.inner.declaration()
    }

    /// Go forwards `ProcessRequest`. The flow packs this wrapper with `pack_tool` first, so
    /// `req.tools` maps the name to the wrapper.
    fn process_request(&self, ctx: &Context, req: &mut LlmRequest) -> Result<(), ToolError> {
        self.inner.process_request(ctx, req)
    }

    fn as_runnable(&self) -> Option<&dyn RunnableTool> {
        (!self.streaming).then_some(self as &dyn RunnableTool)
    }

    fn as_streaming(&self) -> Option<&dyn StreamingTool> {
        self.streaming.then_some(self as &dyn StreamingTool)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[async_trait]
impl RunnableTool for ConfirmationTool {
    /// Go `confirmationTool.Run`.
    async fn run(&self, ctx: Context, args: Value) -> Result<Map<String, Value>, ToolError> {
        confirm_call(
            &ctx,
            self.name(),
            &args,
            self.require_confirmation,
            self.provider.as_ref(),
        )?;
        match self.inner.as_runnable() {
            Some(r) => r.run(ctx, args).await,
            None => Err(ToolError::InvalidArgument(format!(
                "tool {:?} is not runnable",
                self.name()
            ))),
        }
    }
}

impl StreamingTool for ConfirmationTool {
    /// Go `confirmationStreamingTool.RunStream`: the confirmation check is lazy and runs when
    /// iteration starts.
    fn run_stream(
        &self,
        ctx: Context,
        args: Value,
    ) -> BoxStream<'static, Result<String, ToolError>> {
        let (inner, require, provider) = (
            self.inner.clone(),
            self.require_confirmation,
            self.provider.clone(),
        );
        futures::stream::once(async move {
            let name = inner.name().to_string();
            match confirm_call(&ctx, &name, &args, require, provider.as_ref()) {
                Err(e) => futures::stream::once(async move { Err(e) }).boxed(),
                Ok(()) => match inner.as_streaming() {
                    Some(s) => s.run_stream(ctx, args),
                    None => futures::stream::once(async move {
                        Err(ToolError::InvalidArgument(format!(
                            "tool {name:?} is not streaming"
                        )))
                    })
                    .boxed(),
                },
            }
        })
        .flatten()
        .boxed()
    }
}

/// Port of Go `confirmCall`. Go wraps the sentinels as `"error tool %q %w"`; that wrapping is
/// `AgentError::Tool`, added by the flow.
fn confirm_call(
    ctx: &Context,
    tool_name: &str,
    args: &Value,
    require_confirmation: bool,
    provider: Option<&ConfirmationProvider>,
) -> Result<(), ToolError> {
    if let Some(c) = ctx.tool_confirmation() {
        return if c.confirmed {
            Ok(())
        } else {
            Err(ToolError::ConfirmationRejected)
        };
    }
    let require = match provider {
        Some(p) => p(tool_name, args),
        None => require_confirmation,
    };
    if !require {
        return Ok(());
    }
    ctx.request_confirmation(
        &format!(
            "Please approve or reject the tool call {tool_name}() by responding with a FunctionResponse with an expected ToolConfirmation payload."
        ),
        None,
    )?;
    ctx.update_actions(|a| a.skip_summarization = true);
    Err(ToolError::ConfirmationRequired)
}
