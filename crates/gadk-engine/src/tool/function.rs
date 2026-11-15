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

//! Port of Go tool/functiontool/function.go.

use std::any::Any;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;

use async_trait::async_trait;
use futures::FutureExt;
use futures::future::BoxFuture;
use gadk_agent::{Context, RunnableTool, Tool, ToolError, ToolOriginKind};
use gadk_genai::FunctionDeclaration;
use serde_json::{Map, Value};

use crate::tool::utils::{
    CompiledSchema, convert_args_with_schema, convert_with_schema, go_type_name,
    infer_output_schema, infer_schema, panic_message, schema_expects_object, validate_with_schema,
};

/// Go functiontool's long-running declaration note (function.go Declaration).
pub(crate) const LONG_RUNNING_NOTE: &str = "NOTE: This is a long-running operation. Do not call this tool again if it has already returned some intermediate or pending status.";

/// Port of Go functiontool.Config.
///
/// Go's `RequireConfirmationProvider` is the typed `provider` argument of
/// [`FunctionTool::with_confirmation_provider`] / `StreamingFunctionTool::with_confirmation_provider`.
#[derive(Clone)]
pub struct FunctionToolConfig {
    /// The tool name.
    pub name: String,
    /// The tool description sent to the model.
    pub description: String,
    /// Overrides the input schema inferred from the handler's argument type.
    pub input_schema: Option<Value>,
    /// Overrides the output schema inferred from the handler's result type.
    pub output_schema: Option<Value>,
    /// Whether the tool is a long-running operation (adds a note to the declaration).
    pub is_long_running: bool,
    /// Whether every call requires a Human-in-the-Loop confirmation.
    pub require_confirmation: bool,
    /// What [`Tool::origin_kind`] reports for the built tool (default [`ToolOriginKind::Local`],
    /// as Go's analytics plugin classifies a `functiontool` tool, `plugin/agentanalytics/events.go`
    /// `classifyADKToolType`). Go tells a skill tool apart by its argument types' package; gadk's
    /// skill tools set [`ToolOriginKind::Skill`] here instead.
    pub origin: ToolOriginKind,
}

impl Default for FunctionToolConfig {
    /// Every field at its zero value except [`origin`](FunctionToolConfig::origin), which is
    /// [`ToolOriginKind::Local`].
    fn default() -> Self {
        Self {
            name: String::new(),
            description: String::new(),
            input_schema: None,
            output_schema: None,
            is_long_running: false,
            require_confirmation: false,
            origin: ToolOriginKind::Local,
        }
    }
}

impl FunctionToolConfig {
    /// A config with the given name and description and every other field at its default.
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            ..Default::default()
        }
    }
}

type ErasedRun = Arc<
    dyn Fn(Context, Value) -> BoxFuture<'static, Result<Map<String, Value>, ToolError>>
        + Send
        + Sync,
>;

/// Port of Go functiontool's functionTool: a tool wrapping a typed async Rust function.
///
/// The input (and output) JSON schemas are inferred from the handler's argument and result types unless
/// [`FunctionToolConfig`] overrides them. Type-erased: the generic handler lives in `run`.
pub struct FunctionTool {
    cfg: FunctionToolConfig,
    input_schema: Arc<CompiledSchema>,
    output_schema: Arc<CompiledSchema>,
    run: ErasedRun,
}

/// Port of the TArgs kind check in Go functiontool.New / NewStreaming (struct, map, or pointer to those
/// ⇔ the inferred schema's root type is or includes "object"). Checked on the inferred schema even when
/// `cfg.input_schema` overrides it, as Go checks the Go type regardless of the override.
pub(crate) fn check_args_type<Args: schemars::JsonSchema>() -> Result<(), ToolError> {
    if schema_expects_object(&infer_schema::<Args>()) {
        return Ok(());
    }
    Err(ToolError::InvalidArgument(format!(
        "input must be a struct or a map or a pointer to those types, but received: {}: invalid argument",
        std::any::type_name::<Args>()
    )))
}

/// Port of Go functiontool.resolvedSchema: the override when given, else the inferred schema; compiled.
pub(crate) fn resolved_schema(
    override_schema: Option<&Value>,
    inferred: impl FnOnce() -> Value,
    what: &str,
) -> Result<CompiledSchema, ToolError> {
    let schema = override_schema.cloned().unwrap_or_else(inferred);
    CompiledSchema::new(schema)
        .map_err(|e| ToolError::InvalidArgument(format!("failed to infer {what} schema: {e}")))
}

/// Port of Go functiontool Declaration (both variants).
pub(crate) fn declaration(
    cfg: &FunctionToolConfig,
    input: &CompiledSchema,
    output: Option<&CompiledSchema>,
) -> FunctionDeclaration {
    let mut description = cfg.description.clone();
    if cfg.is_long_running {
        if description.is_empty() {
            description = LONG_RUNNING_NOTE.to_string();
        } else {
            description.push_str("\n\n");
            description.push_str(LONG_RUNNING_NOTE);
        }
    }
    FunctionDeclaration {
        name: cfg.name.clone(),
        description,
        parameters_json_schema: Some(input.schema().clone()),
        response_json_schema: output.map(|s| s.schema().clone()),
        ..Default::default()
    }
}

/// Port of the confirmation block of Go functiontool Run / RunStream. `Ok(())` ⇒ proceed to the handler.
///
/// `require` is evaluated only when the context carries no confirmation, as Go runs the
/// `RequireConfirmationProvider` only in that branch.
pub(crate) fn check_confirmation(
    ctx: &Context,
    name: &str,
    require: impl FnOnce() -> bool,
) -> Result<(), ToolError> {
    if let Some(confirmation) = ctx.tool_confirmation() {
        return if confirmation.confirmed {
            Ok(())
        } else {
            Err(ToolError::ConfirmationRejected)
        };
    }
    if !require() {
        return Ok(());
    }
    ctx.request_confirmation(
        &format!(
            "Please approve or reject the tool call {name}() by responding with a FunctionResponse with an expected ToolConfirmation payload."
        ),
        None,
    )?;
    ctx.update_actions(|a| a.skip_summarization = true);
    Err(ToolError::ConfirmationRequired)
}

/// The `json.Unmarshal(raw, &map[string]any)` step of Go typeutil.ConvertToWithJSONSchema in functiontool
/// Run: a `null` that passed the output schema unmarshals to a nil (empty) map, not an error.
fn result_map(v: Value) -> Result<Map<String, Value>, String> {
    match v {
        Value::Null => Ok(Map::new()),
        v => serde_json::from_value(v).map_err(|e| e.to_string()),
    }
}

/// Port of Go's "unexpected args type, got: %T" check: an object or `null` (Go: a typed nil map) is accepted.
pub(crate) fn check_args_value(args: &Value) -> Result<(), ToolError> {
    match args {
        Value::Object(_) | Value::Null => Ok(()),
        other => Err(ToolError::InvalidArgument(format!(
            "unexpected args type, got: {}",
            go_type_name(other)
        ))),
    }
}

impl FunctionTool {
    /// Port of Go functiontool.New: a tool calling `f` with arguments decoded (and validated) as `Args`.
    ///
    /// Fails with [`ToolError::InvalidArgument`] when `Args` is not an object type or a schema cannot be
    /// compiled.
    #[allow(clippy::new_ret_no_self)] // mirrors Go functiontool.New, which returns tool.Tool (C4)
    pub fn new<Args, Out, F, Fut>(cfg: FunctionToolConfig, f: F) -> Result<Arc<dyn Tool>, ToolError>
    where
        Args: serde::de::DeserializeOwned + schemars::JsonSchema + Send + 'static,
        Out: serde::Serialize + schemars::JsonSchema + Send + 'static,
        F: Fn(Context, Args) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<Out, ToolError>> + Send + 'static,
    {
        Self::build::<Args, Out, F, Fut, fn(&Args) -> bool>(cfg, f, None)
    }

    /// Like [`FunctionTool::new`], with Go's `Config.RequireConfirmationProvider`: when the call carries no
    /// confirmation, `provider(&args)` decides whether one is required, overriding
    /// `cfg.require_confirmation`.
    pub fn with_confirmation_provider<Args, Out, F, Fut, P>(
        cfg: FunctionToolConfig,
        f: F,
        provider: P,
    ) -> Result<Arc<dyn Tool>, ToolError>
    where
        Args: serde::de::DeserializeOwned + schemars::JsonSchema + Send + 'static,
        Out: serde::Serialize + schemars::JsonSchema + Send + 'static,
        F: Fn(Context, Args) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<Out, ToolError>> + Send + 'static,
        P: Fn(&Args) -> bool + Send + Sync + 'static,
    {
        Self::build(cfg, f, Some(provider))
    }

    fn build<Args, Out, F, Fut, P>(
        cfg: FunctionToolConfig,
        f: F,
        provider: Option<P>,
    ) -> Result<Arc<dyn Tool>, ToolError>
    where
        Args: serde::de::DeserializeOwned + schemars::JsonSchema + Send + 'static,
        Out: serde::Serialize + schemars::JsonSchema + Send + 'static,
        F: Fn(Context, Args) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<Out, ToolError>> + Send + 'static,
        P: Fn(&Args) -> bool + Send + Sync + 'static,
    {
        check_args_type::<Args>()?;
        let input_schema = Arc::new(resolved_schema(
            cfg.input_schema.as_ref(),
            infer_schema::<Args>,
            "input",
        )?);
        let output_schema = Arc::new(resolved_schema(
            cfg.output_schema.as_ref(),
            infer_output_schema::<Out>,
            "output",
        )?);

        let f = Arc::new(f);
        let provider = provider.map(Arc::new);
        let name = cfg.name.clone();
        let require_confirmation = cfg.require_confirmation;
        let explicit_output = cfg.output_schema.is_some();
        let (in_schema, out_schema) = (input_schema.clone(), output_schema.clone());
        let run: ErasedRun = Arc::new(move |ctx: Context, args: Value| {
            let (f, provider, name) = (f.clone(), provider.clone(), name.clone());
            let (input_schema, output_schema) = (in_schema.clone(), out_schema.clone());
            async move {
                check_args_value(&args)?;
                let input: Args = convert_args_with_schema(&args, Some(&input_schema))
                    .map_err(ToolError::InvalidArgument)?;
                check_confirmation(&ctx, &name, || {
                    provider
                        .as_ref()
                        .map_or(require_confirmation, |p| p(&input))
                })?;
                let output: Out = f(ctx, input).await?;
                let conv_err =
                    match convert_with_schema::<Out, Value>(&output, Some(&output_schema))
                        .and_then(result_map)
                    {
                        Ok(m) => return Ok(m),
                        Err(e) => e,
                    };
                // Go: a result encoding/json cannot represent must not be wrapped, because it cannot be sent.
                let value =
                    serde_json::to_value(&output).map_err(|e| ToolError::Execution(e.into()))?;
                // Go: a schema inferred from TResults describes the type, not what encoding/json emits for it.
                if explicit_output && validate_with_schema(&output, Some(&output_schema)).is_err() {
                    return Err(ToolError::Execution(conv_err.into()));
                }
                Ok(Map::from_iter([("result".to_string(), value)]))
            }
            .boxed()
        });
        Ok(Arc::new(FunctionTool {
            cfg,
            input_schema,
            output_schema,
            run,
        }))
    }
}

impl Tool for FunctionTool {
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
        Some(declaration(
            &self.cfg,
            &self.input_schema,
            Some(&self.output_schema),
        ))
    }

    // `process_request` is deliberately not overridden: Go's ProcessRequest is toolutils.PackTool, which
    // the flow runs via `gadk_agent::pack_tool` before calling `Tool::process_request`.

    fn as_runnable(&self) -> Option<&dyn RunnableTool> {
        Some(self)
    }

    /// [`FunctionToolConfig::origin`] (default [`ToolOriginKind::Local`]).
    fn origin_kind(&self) -> ToolOriginKind {
        self.cfg.origin
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[async_trait]
impl RunnableTool for FunctionTool {
    /// Port of Go functiontool Run, including its recover(): a panic becomes [`ToolError::Panic`] with Go's
    /// "panic in tool %q: %v" text (Go appends "\nstack: …"; Rust cannot capture the panicking stack after
    /// unwinding).
    async fn run(&self, ctx: Context, args: Value) -> Result<Map<String, Value>, ToolError> {
        match AssertUnwindSafe((self.run)(ctx, args)).catch_unwind().await {
            Ok(result) => result,
            Err(payload) => Err(ToolError::Panic(format!(
                "panic in tool {}: {}",
                gadk_agent::go_quote(&self.cfg.name),
                panic_message(&*payload)
            ))),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod origin_tests {
    use super::*;

    #[derive(serde::Deserialize, schemars::JsonSchema)]
    struct NoArgs {}

    fn tool(cfg: FunctionToolConfig) -> Arc<dyn Tool> {
        FunctionTool::new(cfg, |_ctx: Context, _args: NoArgs| async {
            Ok::<_, ToolError>(String::new())
        })
        .unwrap()
    }

    #[test]
    fn config_origin_defaults_to_local() {
        assert_eq!(
            FunctionToolConfig::new("t", "").origin,
            ToolOriginKind::Local
        );
        assert_eq!(FunctionToolConfig::default().origin, ToolOriginKind::Local);
    }

    #[test]
    fn function_tool_reports_its_configured_origin() {
        let local = tool(FunctionToolConfig::new("t", ""));
        assert_eq!(local.origin_kind(), ToolOriginKind::Local);
        assert!(local.origin_agent().is_none());
        assert!(local.unwrap().is_none());
        let skill = tool(FunctionToolConfig {
            origin: ToolOriginKind::Skill,
            ..FunctionToolConfig::new("t", "")
        });
        assert_eq!(skill.origin_kind(), ToolOriginKind::Skill);
    }
}
