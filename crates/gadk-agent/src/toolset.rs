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

use crate::go_fmt::go_quote;
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

#[async_trait]
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

    /// Go forwards `ProcessRequest`. The flow packs this wrapper with `pack_tool` first (when it
    /// has a declaration), so `req.tools` maps the name to the wrapper.
    async fn process_request(&self, ctx: &Context, req: &mut LlmRequest) -> Result<(), ToolError> {
        self.inner.process_request(ctx, req).await
    }

    fn as_runnable(&self) -> Option<&dyn RunnableTool> {
        (!self.streaming).then_some(self as &dyn RunnableTool)
    }

    fn as_streaming(&self) -> Option<&dyn StreamingTool> {
        self.streaming.then_some(self as &dyn StreamingTool)
    }

    /// Go's analytics plugin unwraps `confirmationTool` through its inner field
    /// (`plugin/agentanalytics/events.go` `unwrapToolValue`).
    fn unwrap(&self) -> Option<Arc<dyn Tool>> {
        Some(self.inner.clone())
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
                "tool {} is not runnable",
                go_quote(self.name())
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
                            "tool {} is not streaming",
                            go_quote(&name)
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

/// Port of Go `confirmCall`. Go wraps the sentinels as `"error tool %q %w"`; here they are returned
/// bare and the flow adds that wrapping (gadk-engine `llm/functions.rs` `wrap_confirmation_error`,
/// a `ToolError::Execution(NamedToolError)`).
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::any::Any;
    use std::sync::Arc;

    use async_trait::async_trait;
    use serde_json::{Map, Value};

    use super::with_confirmation;
    use crate::context::{Context, ReadonlyContext};
    use crate::error::ToolError;
    use crate::tool::{RunnableTool, Tool, ToolOriginKind, Toolset};

    /// A runnable tool reporting `Local`.
    struct Local;

    #[async_trait]
    impl Tool for Local {
        fn name(&self) -> &str {
            "local"
        }
        fn description(&self) -> &str {
            ""
        }
        fn as_runnable(&self) -> Option<&dyn RunnableTool> {
            Some(self)
        }
        fn origin_kind(&self) -> ToolOriginKind {
            ToolOriginKind::Local
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    #[async_trait]
    impl RunnableTool for Local {
        async fn run(&self, _ctx: Context, _args: Value) -> Result<Map<String, Value>, ToolError> {
            Ok(Map::new())
        }
    }

    struct One(Arc<dyn Tool>);

    #[async_trait]
    impl Toolset for One {
        fn name(&self) -> &str {
            "one"
        }
        async fn tools(&self, _ctx: &ReadonlyContext) -> Result<Vec<Arc<dyn Tool>>, ToolError> {
            Ok(vec![self.0.clone()])
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    #[test]
    fn origin_defaults() {
        let t = Local;
        assert!(t.unwrap().is_none());
        assert!(t.origin_agent().is_none());
        assert!(t.origin_agents().is_empty());
        assert_eq!(t.origin_kind(), ToolOriginKind::Local);
        assert_eq!(ToolOriginKind::default(), ToolOriginKind::Unknown);
    }

    #[tokio::test]
    async fn confirmation_wrapper_unwraps_to_the_inner_tool() {
        let inner: Arc<dyn Tool> = Arc::new(Local);
        let ts = with_confirmation(Arc::new(One(inner.clone())), true, None);
        let ctx = Context::builder().build();
        let wrapped = ts.tools(ctx.as_readonly()).await.unwrap().remove(0);
        assert!(!Arc::ptr_eq(&wrapped, &inner), "the tool is wrapped");
        assert_eq!(
            wrapped.origin_kind(),
            ToolOriginKind::Unknown,
            "the wrapper does not forward its origin; consumers unwrap"
        );
        let unwrapped = wrapped.unwrap().expect("wrapper unwraps");
        assert!(Arc::ptr_eq(&unwrapped, &inner));
        assert_eq!(unwrapped.origin_kind(), ToolOriginKind::Local);
        assert!(unwrapped.unwrap().is_none());
    }
}
