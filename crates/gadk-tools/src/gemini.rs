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

//! Gemini native tools.
//!
//! [`new`] wraps any [`gadk_genai::Tool`] (for example a retrieval tool). The unit structs
//! [`GoogleSearch`], [`GoogleMaps`], [`UrlContext`] and [`EnterpriseWebSearch`] are the default
//! built-ins. None of them declares a function, so the flow never packs them into
//! `LlmRequest::tools`. Each one adds its `genai::Tool` to `req.config.tools` in
//! `process_request`, after its model checks.

use std::any::Any;
use std::sync::Arc;

use async_trait::async_trait;
use gadk_agent::{Context, Tool, ToolError};
use gadk_genai::GenerateContentConfig;
use gadk_model::{LlmRequest, is_gemini_model, is_gemini1_model};

/// A tool that adds `tool` to every model request.
pub fn new(
    name: impl Into<String>,
    description: impl Into<String>,
    tool: gadk_genai::Tool,
) -> Arc<dyn Tool> {
    Arc::new(GeminiTool {
        name: name.into(),
        description: description.into(),
        value: tool,
    })
}

/// A wrapper around a [`gadk_genai::Tool`].
struct GeminiTool {
    name: String,
    description: String,
    value: gadk_genai::Tool,
}

#[async_trait]
impl Tool for GeminiTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    /// Adds the wrapped tool to `req.config.tools`.
    async fn process_request(&self, _ctx: &Context, req: &mut LlmRequest) -> Result<(), ToolError> {
        set_tool(req, self.value.clone());
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Adds `t` to `req.config.tools`, creating the config when there is none.
fn set_tool(req: &mut LlmRequest, t: gadk_genai::Tool) {
    req.config
        .get_or_insert_with(GenerateContentConfig::default)
        .tools
        .push(t);
}

/// The error for a tool that does not support `model`:
/// `cannot process: <tool> tool is not supported for model <model>`.
fn unsupported(tool: &str, model: &str) -> ToolError {
    ToolError::InvalidArgument(format!(
        "cannot process: {tool} tool is not supported for model {model}"
    ))
}

/// A built-in tool that Gemini 2 models invoke automatically to retrieve Google Search results. It
/// runs inside the model and executes nothing locally.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GoogleSearch;

#[async_trait]
impl Tool for GoogleSearch {
    fn name(&self) -> &str {
        "google_search"
    }

    fn description(&self) -> &str {
        "Performs a Google search to retrieve information from the web."
    }

    /// Adds Google Search to the request. A Gemini 1.x model is rejected only when the request
    /// already carries function tools (`req.tools`).
    async fn process_request(&self, _ctx: &Context, req: &mut LlmRequest) -> Result<(), ToolError> {
        if !is_gemini_model(&req.model) {
            return Err(unsupported("Google search", &req.model));
        }
        if is_gemini1_model(&req.model) && !req.tools.is_empty() {
            return Err(ToolError::InvalidArgument(
                "cannot process: Google search tool cannot be used with other tools in Gemini 1.x"
                    .into(),
            ));
        }
        set_tool(
            req,
            gadk_genai::Tool {
                google_search: Some(gadk_genai::GoogleSearch::default()),
                ..Default::default()
            },
        );
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// A built-in tool that Gemini 2 models invoke automatically to retrieve Google Maps results.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GoogleMaps;

#[async_trait]
impl Tool for GoogleMaps {
    fn name(&self) -> &str {
        "google_maps"
    }

    fn description(&self) -> &str {
        "Performs a Google map search to retrieve geographical information from the web."
    }

    /// Adds Google Maps to the request. Every Gemini 1.x model is rejected.
    async fn process_request(&self, _ctx: &Context, req: &mut LlmRequest) -> Result<(), ToolError> {
        if !is_gemini_model(&req.model) {
            return Err(unsupported("Google maps", &req.model));
        }
        if is_gemini1_model(&req.model) {
            return Err(ToolError::InvalidArgument(
                "cannot process: Google maps tool cannot be used with other tools in Gemini 1.x"
                    .into(),
            ));
        }
        set_tool(
            req,
            gadk_genai::Tool {
                google_maps: Some(gadk_genai::GoogleMaps::default()),
                ..Default::default()
            },
        );
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// A built-in tool that Gemini 2 models invoke automatically to retrieve the content of web pages.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UrlContext;

#[async_trait]
impl Tool for UrlContext {
    fn name(&self) -> &str {
        "url_context"
    }

    fn description(&self) -> &str {
        "Allows to access the content from web pages."
    }

    /// Adds URL context to the request. Every Gemini 1.x model is rejected.
    async fn process_request(&self, _ctx: &Context, req: &mut LlmRequest) -> Result<(), ToolError> {
        if !is_gemini_model(&req.model) {
            return Err(unsupported("Url context", &req.model));
        }
        if is_gemini1_model(&req.model) {
            return Err(ToolError::InvalidArgument(
                "cannot process: Url context tool cannot be used in Gemini 1.x".into(),
            ));
        }
        set_tool(
            req,
            gadk_genai::Tool {
                url_context: Some(gadk_genai::URLContext::default()),
                ..Default::default()
            },
        );
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// A built-in tool that grounds responses in web search results under Enterprise compliance
/// controls. It is a model built-in, never executed locally. It is not Vertex AI Search (formerly
/// "Enterprise Search").
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EnterpriseWebSearch;

#[async_trait]
impl Tool for EnterpriseWebSearch {
    fn name(&self) -> &str {
        "enterprise_web_search"
    }

    fn description(&self) -> &str {
        "Performs web grounding for Enterprise compliance"
    }

    /// Adds Enterprise web search to the request. The config is created first. A Gemini 1.x model
    /// is rejected only when `req.config.tools` is non-empty.
    async fn process_request(&self, _ctx: &Context, req: &mut LlmRequest) -> Result<(), ToolError> {
        let has_other_tools = !req
            .config
            .get_or_insert_with(GenerateContentConfig::default)
            .tools
            .is_empty();
        if !is_gemini_model(&req.model) {
            return Err(unsupported("Enterprise Web Search", &req.model));
        }
        if is_gemini1_model(&req.model) && has_other_tools {
            return Err(ToolError::InvalidArgument(
                "cannot process: Enterprise Web Search tool cannot be used with other tools in Gemini 1.x"
                    .into(),
            ));
        }
        set_tool(
            req,
            gadk_genai::Tool {
                enterprise_web_search: Some(gadk_genai::EnterpriseWebSearch::default()),
                ..Default::default()
            },
        );
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
