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

//! One MCP server tool as a gadk tool (port of ADK-Go `tool/mcptoolset/tool.go` `convertTool`,
//! `mcpTool`).

use std::any::Any;
use std::sync::Arc;

use gadk_agent::Context;
use gadk_agent::ToolError;
use gadk_agent::tool::{ConfirmationProvider, RunnableTool, Tool};
use gadk_auth::AuthContext;
use gadk_genai::FunctionDeclaration;
use rmcp::model::{CallToolRequestParams, MetaObject, RequestMetaObject};
use serde_json::{Map, Value};

use crate::client::{CallContext, McpClient};
use crate::config::MetadataProvider;
use crate::error::Error;
use crate::format::format_mcp_content;

/// Go `mcpTool`: an MCP server tool. Its declaration carries the server's input and output
/// schemas unchanged; running it calls `tools/call` on the toolset's session.
pub struct McpTool {
    name: String,
    description: String,
    declaration: FunctionDeclaration,
    client: Arc<dyn McpClient>,
    require_confirmation: bool,
    provider: Option<ConfirmationProvider>,
    metadata: Option<MetadataProvider>,
}

impl McpTool {
    /// Go `convertTool`.
    #[allow(dead_code)] // called by Task 9's `McpToolset::tools`
    pub(crate) fn new(
        tool: rmcp::model::Tool,
        client: Arc<dyn McpClient>,
        require_confirmation: bool,
        provider: Option<ConfirmationProvider>,
        metadata: Option<MetadataProvider>,
    ) -> Self {
        let name = tool.name.to_string();
        let description = tool
            .description
            .as_deref()
            .map(str::to_string)
            .unwrap_or_default();
        // rmcp always decodes `inputSchema` (it is required on the wire), so Go's
        // `InputSchema != nil` always holds; `outputSchema` is optional in both.
        let declaration = FunctionDeclaration {
            name: name.clone(),
            description: description.clone(),
            parameters_json_schema: Some(Value::Object((*tool.input_schema).clone())),
            response_json_schema: tool
                .output_schema
                .as_deref()
                .map(|schema| Value::Object(schema.clone())),
            ..Default::default()
        };
        Self {
            name,
            description,
            declaration,
            client,
            require_confirmation,
            provider,
            metadata,
        }
    }

    /// Go's `fmt.Errorf("… %w", err)` texts reach the flow as `ToolError::Execution` sources.
    fn execution(err: Error) -> ToolError {
        ToolError::Execution(Box::new(err))
    }

    /// Go `mcpTool.Run`'s `MetadataProvider` step: the provider's map, checked for reserved keys
    /// in sorted order and copied. `None` (and an empty map) contribute nothing.
    fn request_meta(&self, ctx: &Context) -> Result<Option<RequestMetaObject>, Error> {
        let Some(provider) = &self.metadata else {
            return Ok(None);
        };
        let meta = provider(ctx).map_err(|source| Error::MetadataProvider {
            name: self.name.clone(),
            source,
        })?;
        let Some(meta) = meta else {
            return Ok(None);
        };
        let mut keys: Vec<&String> = meta.keys().collect();
        keys.sort();
        if let Some(key) = keys
            .into_iter()
            .find(|k| *k == "progressToken" || k.starts_with("io.modelcontextprotocol/"))
        {
            return Err(Error::ReservedMetaKey {
                name: self.name.clone(),
                key: key.clone(),
            });
        }
        // Go `maps.Clone`: the provider's map is never written to. go-sdk omits an empty `_meta`.
        if meta.is_empty() {
            return Ok(None);
        }
        Ok(Some(RequestMetaObject(MetaObject(meta))))
    }
}

#[async_trait::async_trait]
impl Tool for McpTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    /// The declaration: name, description, `parameters_json_schema` = the MCP `inputSchema`,
    /// `response_json_schema` = the MCP `outputSchema` when present.
    fn declaration(&self) -> Option<FunctionDeclaration> {
        Some(self.declaration.clone())
    }

    fn as_runnable(&self) -> Option<&dyn RunnableTool> {
        Some(self)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[async_trait::async_trait]
impl RunnableTool for McpTool {
    /// Go `mcpTool.Run`: confirmation, metadata, `tools/call`, result shaping.
    async fn run(&self, ctx: Context, args: Value) -> Result<Map<String, Value>, ToolError> {
        match ctx.tool_confirmation() {
            Some(confirmation) => {
                if !confirmation.confirmed {
                    return Err(ToolError::ConfirmationRejected);
                }
            }
            None => {
                // The provider, when set, overrides the static flag.
                let require = match &self.provider {
                    Some(provider) => provider(&self.name, &args),
                    None => self.require_confirmation,
                };
                if require {
                    // `request_confirmation` also sets `skip_summarization` (Go sets it here).
                    ctx.request_confirmation(
                        &format!(
                            "Please approve or reject the tool call {}() by responding with a FunctionResponse with an expected ToolConfirmation payload.",
                            self.name
                        ),
                        None,
                    )?;
                    return Err(ToolError::ConfirmationRequired);
                }
            }
        }

        let mut params = CallToolRequestParams::new(self.name.clone());
        // rmcp's `arguments` is a JSON object; a non-object `args` (never produced by the flow,
        // which decodes function-call args as an object) is sent as no arguments.
        if let Value::Object(arguments) = args {
            params.arguments = Some(arguments);
        }
        params.meta = self.request_meta(&ctx).map_err(Self::execution)?;

        // Same as `CallContext::from(&ctx)`: the invocation's identity and cancellation token.
        let cx = CallContext::new(AuthContext::from(&ctx), ctx.cancellation_token().clone());
        let res = self.client.call_tool(&cx, params).await.map_err(|err| {
            Self::execution(Error::CallTool {
                name: self.name.clone(),
                source: Box::new(err),
            })
        })?;

        if res.is_error == Some(true) {
            let details = format_mcp_content(&res.content);
            let mut msg = String::from("Tool execution failed.");
            if !details.is_empty() {
                msg.push_str(" Details: ");
                msg.push_str(&details);
            }
            return Err(Self::execution(Error::ToolExecution(msg)));
        }

        let output = match res.structured_content {
            // Go `StructuredContent != nil`: a JSON `null` decodes to nil.
            Some(structured) if !structured.is_null() => structured,
            _ => Value::String(format_mcp_content(&res.content)),
        };
        let mut result = Map::new();
        result.insert("output".to_string(), output);
        Ok(result)
    }
}

#[cfg(test)]
#[path = "tool_tests.rs"]
mod tests;
