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

//! Port of Go tool/functiontool/streaming_function.go.

use std::any::Any;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;

use futures::stream::BoxStream;
use futures::{StreamExt, future, stream};
use gadk_agent::{Context, StreamingTool, Tool, ToolError, ToolOriginKind};
use gadk_genai::FunctionDeclaration;
use serde_json::Value;

use crate::tool::function::{
    FunctionToolConfig, check_args_type, check_args_value, check_confirmation, declaration,
    resolved_schema,
};
use crate::tool::utils::{CompiledSchema, convert_with_schema, infer_schema, panic_message};

type ErasedStart = Arc<
    dyn Fn(Context, Value) -> Result<BoxStream<'static, Result<String, ToolError>>, ToolError>
        + Send
        + Sync,
>;

/// Port of Go functiontool's streamingFunctionTool: a tool wrapping a Rust function that returns a stream of
/// text chunks.
pub struct StreamingFunctionTool {
    cfg: FunctionToolConfig,
    input_schema: Arc<CompiledSchema>,
    start: ErasedStart,
}

impl StreamingFunctionTool {
    /// Port of Go functiontool.NewStreaming.
    ///
    /// Fails with [`ToolError::InvalidArgument`] when `Args` is not an object type or the input schema cannot
    /// be compiled.
    #[allow(clippy::new_ret_no_self)] // mirrors Go functiontool.NewStreaming, which returns tool.Tool (C4)
    pub fn new<Args, F, S>(cfg: FunctionToolConfig, f: F) -> Result<Arc<dyn Tool>, ToolError>
    where
        Args: serde::de::DeserializeOwned + schemars::JsonSchema + Send + 'static,
        F: Fn(Context, Args) -> S + Send + Sync + 'static,
        S: futures::Stream<Item = Result<String, ToolError>> + Send + 'static,
    {
        Self::build::<Args, F, S, fn(&Args) -> bool>(cfg, f, None)
    }

    /// Like [`StreamingFunctionTool::new`], with Go's `Config.RequireConfirmationProvider`: when the call
    /// carries no confirmation, `provider(&args)` decides whether one is required, overriding
    /// `cfg.require_confirmation`.
    pub fn with_confirmation_provider<Args, F, S, P>(
        cfg: FunctionToolConfig,
        f: F,
        provider: P,
    ) -> Result<Arc<dyn Tool>, ToolError>
    where
        Args: serde::de::DeserializeOwned + schemars::JsonSchema + Send + 'static,
        F: Fn(Context, Args) -> S + Send + Sync + 'static,
        S: futures::Stream<Item = Result<String, ToolError>> + Send + 'static,
        P: Fn(&Args) -> bool + Send + Sync + 'static,
    {
        Self::build(cfg, f, Some(provider))
    }

    fn build<Args, F, S, P>(
        cfg: FunctionToolConfig,
        f: F,
        provider: Option<P>,
    ) -> Result<Arc<dyn Tool>, ToolError>
    where
        Args: serde::de::DeserializeOwned + schemars::JsonSchema + Send + 'static,
        F: Fn(Context, Args) -> S + Send + Sync + 'static,
        S: futures::Stream<Item = Result<String, ToolError>> + Send + 'static,
        P: Fn(&Args) -> bool + Send + Sync + 'static,
    {
        check_args_type::<Args>()?;
        let input_schema = Arc::new(resolved_schema(
            cfg.input_schema.as_ref(),
            infer_schema::<Args>,
            "input",
        )?);

        let name = cfg.name.clone();
        let require_confirmation = cfg.require_confirmation;
        let schema = input_schema.clone();
        // The synchronous prelude of Go RunStream: argument checks, conversion, confirmation, handler call.
        let start: ErasedStart = Arc::new(move |ctx: Context, args: Value| {
            check_args_value(&args)?;
            let input: Args =
                convert_with_schema(&args, Some(&schema)).map_err(ToolError::InvalidArgument)?;
            check_confirmation(&ctx, &name, || {
                provider
                    .as_ref()
                    .map_or(require_confirmation, |p| p(&input))
            })?;
            Ok(f(ctx, input).boxed())
        });
        Ok(Arc::new(StreamingFunctionTool {
            cfg,
            input_schema,
            start,
        }))
    }
}

impl Tool for StreamingFunctionTool {
    fn name(&self) -> &str {
        &self.cfg.name
    }

    fn description(&self) -> &str {
        &self.cfg.description
    }

    fn is_long_running(&self) -> bool {
        self.cfg.is_long_running
    }

    fn declaration(&self) -> Option<FunctionDeclaration> {
        Some(declaration(&self.cfg, &self.input_schema, None))
    }

    // `process_request` is deliberately not overridden: Go's ProcessRequest is toolutils.PackTool, which
    // the flow runs via `gadk_agent::pack_tool` before calling `Tool::process_request`.

    fn as_streaming(&self) -> Option<&dyn StreamingTool> {
        Some(self)
    }

    /// [`FunctionToolConfig::origin`] (default [`ToolOriginKind::Local`]: Go's streaming function
    /// tool is a `functiontool` type too).
    fn origin_kind(&self) -> ToolOriginKind {
        self.cfg.origin
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl StreamingTool for StreamingFunctionTool {
    /// Port of Go streamingFunctionTool.RunStream.
    /// - Lazy like Go's iterator: nothing runs until the first poll.
    /// - A panic while calling the handler or while polling its stream becomes one final
    ///   `ToolError::Panic("panic in tool %q: %v")` item, after which the stream ends (Go recover + yield).
    /// - Dropping the returned stream drops the handler's stream (Go: yield returns false → handler stops).
    /// - Consumer panics and panics raised while *dropping* the handler stream are never caught: the
    ///   consumer is not called from inside this stream, and Drop is outside catch_unwind's poll scope
    ///   (Go "consumer panics must propagate").
    fn run_stream(
        &self,
        ctx: Context,
        args: Value,
    ) -> BoxStream<'static, Result<String, ToolError>> {
        let start = self.start.clone();
        let name = self.cfg.name.clone();
        let body =
            stream::once(async move { start(ctx, args) }).flat_map(|prelude| match prelude {
                Ok(s) => s,
                Err(e) => stream::once(future::ready(Err(e))).boxed(),
            });
        AssertUnwindSafe(body)
            .catch_unwind()
            .map(move |item| match item {
                Ok(item) => item,
                Err(payload) => Err(ToolError::Panic(format!(
                    "panic in tool {}: {}",
                    gadk_agent::go_quote(&name),
                    panic_message(&*payload)
                ))),
            })
            .boxed()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod origin_tests {
    use super::*;

    #[derive(serde::Deserialize, schemars::JsonSchema)]
    struct NoArgs {}

    fn tool(cfg: FunctionToolConfig) -> Arc<dyn Tool> {
        StreamingFunctionTool::new(cfg, |_ctx: Context, _args: NoArgs| {
            stream::empty::<Result<String, ToolError>>()
        })
        .unwrap()
    }

    #[test]
    fn streaming_function_tool_reports_its_configured_origin() {
        let local = tool(FunctionToolConfig::new("s", ""));
        assert_eq!(local.origin_kind(), ToolOriginKind::Local);
        assert!(local.origin_agent().is_none());
        let skill = tool(FunctionToolConfig {
            origin: ToolOriginKind::Skill,
            ..FunctionToolConfig::new("s", "")
        });
        assert_eq!(skill.origin_kind(), ToolOriginKind::Skill);
    }
}
