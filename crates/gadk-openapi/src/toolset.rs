// Copyright 2026 Chris Olson
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

//! The OpenAPI toolset ([`OpenApiToolset`]).

use std::any::Any;
use std::fmt;
use std::sync::Arc;

use gadk_agent::tool::{Tool, ToolPredicate, Toolset};
use gadk_agent::{ReadonlyContext, ToolError};
use serde_json::Value;
use url::Url;

use crate::config::OpenApiConfig;
use crate::error::Error;
use crate::operation::{self, MAX_NAME_BYTES, SkippedOperation};
use crate::spec::OpenApiSpec;
use crate::tool::{RestApiTool, Shared};

/// The operations of one OpenAPI document as a [`Toolset`] named `openapi_toolset`: one tool
/// per operation, in document order.
///
/// Each tool's declaration lists the operation's path, query and header parameters and, for an
/// `application/json` request body, `body`. A call returns `{"status": <u16>, "body": <body>}`
/// for any HTTP status, plus `"truncated": true` when the body was cut.
pub struct OpenApiToolset {
    tools: Vec<RestApiTool>,
    skipped: Vec<SkippedOperation>,
    prefix: String,
    tool_filter: Option<ToolPredicate>,
}

impl OpenApiToolset {
    /// Builds every tool of `spec` once.
    ///
    /// Fails with [`Error::DuplicateName`] when two operations (or two properties of one tool)
    /// share a name, [`Error::NameTooLong`] when a prefixed name is over 64 bytes,
    /// [`Error::Ref`] for a `$ref` that cannot be resolved, and [`Error::NoBaseUrl`] when there
    /// is no absolute base URL.
    pub fn new(spec: &OpenApiSpec, config: OpenApiConfig) -> Result<Self, Error> {
        let collected = operation::collect(spec)?;
        if !config.prefix.is_empty() {
            for op in &collected.operations {
                let name = format!("{}_{}", config.prefix, op.name);
                if name.len() > MAX_NAME_BYTES {
                    return Err(Error::NameTooLong { name });
                }
            }
        }
        let shared = Arc::new(Shared {
            client: reqwest::Client::new(),
            base_url: base_url(spec, config.base_url.as_deref())?,
            auth: config.auth,
            query_api_key: config.query_api_key,
            headers: config.headers,
            timeout: config.timeout,
            max_response_bytes: config.max_response_bytes,
            require_confirmation: config.require_confirmation,
            confirmation_provider: config.confirmation_provider,
        });
        let tools = collected
            .operations
            .into_iter()
            .map(|op| RestApiTool::new(op, shared.clone()))
            .collect();
        Ok(Self {
            tools,
            skipped: collected.skipped,
            prefix: config.prefix,
            tool_filter: config.tool_filter,
        })
    }

    /// The operations that did not become tools, and why.
    pub fn skipped_operations(&self) -> &[SkippedOperation] {
        &self.skipped
    }
}

impl fmt::Debug for OpenApiToolset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpenApiToolset")
            .field(
                "tools",
                &self
                    .tools
                    .iter()
                    .map(RestApiTool::original_name)
                    .collect::<Vec<_>>(),
            )
            .field("skipped", &self.skipped)
            .field("prefix", &self.prefix)
            .finish_non_exhaustive()
    }
}

#[async_trait::async_trait]
impl Toolset for OpenApiToolset {
    /// `openapi_toolset`.
    fn name(&self) -> &str {
        "openapi_toolset"
    }

    /// The tools in document order, filtered by `tool_filter` (which sees the unprefixed
    /// names), then renamed `<prefix>_<name>` when `prefix` is set.
    async fn tools(&self, ctx: &ReadonlyContext) -> Result<Vec<Arc<dyn Tool>>, ToolError> {
        let mut tools: Vec<Arc<dyn Tool>> = Vec::with_capacity(self.tools.len());
        for tool in &self.tools {
            if let Some(filter) = &self.tool_filter
                && !filter(ctx, tool)
            {
                continue;
            }
            let mut tool = tool.clone();
            if !self.prefix.is_empty() {
                tool.expose_as(format!("{}_{}", self.prefix, tool.original_name()));
            }
            tools.push(Arc::new(tool));
        }
        Ok(tools)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// The base URL without a trailing `/`: `configured`, else `servers[0].url` with its variables
/// set to their defaults and resolved against the URL the document was fetched from.
pub(crate) fn base_url(spec: &OpenApiSpec, configured: Option<&str>) -> Result<String, Error> {
    let url = match configured {
        Some(configured) => Url::parse(configured).map_err(|_| Error::NoBaseUrl)?,
        None => {
            let server = spec.document().get("servers").and_then(|s| s.get(0));
            let mut raw = server
                .and_then(|s| s.get("url"))
                .and_then(Value::as_str)
                .filter(|url| !url.is_empty())
                .unwrap_or("/")
                .to_string();
            if let Some(variables) = server
                .and_then(|s| s.get("variables"))
                .and_then(Value::as_object)
            {
                for (name, variable) in variables {
                    if let Some(default) = variable.get("default").and_then(Value::as_str) {
                        raw = raw.replace(&format!("{{{name}}}"), default);
                    }
                }
            }
            match Url::parse(&raw) {
                Ok(url) => url,
                Err(url::ParseError::RelativeUrlWithoutBase) => match spec.source() {
                    Some(source) => source.join(&raw).map_err(|_| Error::NoBaseUrl)?,
                    None => return Err(Error::NoBaseUrl),
                },
                Err(_) => return Err(Error::NoBaseUrl),
            }
        }
    };
    if url.cannot_be_a_base() {
        return Err(Error::NoBaseUrl);
    }
    Ok(url.as_str().trim_end_matches('/').to_string())
}

#[cfg(test)]
#[path = "toolset_tests.rs"]
mod tests;
