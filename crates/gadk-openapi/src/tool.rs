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

//! One operation as a gadk tool (`RestApiTool`).

use std::any::Any;
use std::sync::Arc;
use std::time::Duration;

use gadk_agent::tool::{ConfirmationProvider, RunnableTool, Tool};
use gadk_agent::{Context, ToolError};
use gadk_auth::{AuthContext, CredentialProvider};
use gadk_genai::FunctionDeclaration;
use serde_json::{Map, Value};

use crate::config::RequireConfirmation;
use crate::error::Error;
use crate::operation::{Operation, is_json};
use crate::request::{self, Parts};

/// What every tool of one toolset shares.
pub(crate) struct Shared {
    pub(crate) client: reqwest::Client,
    /// The base URL without a trailing `/`.
    pub(crate) base_url: String,
    pub(crate) auth: Option<Arc<dyn CredentialProvider>>,
    pub(crate) query_api_key: Option<(String, String)>,
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) timeout: Duration,
    pub(crate) max_response_bytes: usize,
    pub(crate) require_confirmation: RequireConfirmation,
    pub(crate) confirmation_provider: Option<ConfirmationProvider>,
}

/// One operation as a tool. Cloning is cheap (the operation and the shared state are
/// reference-counted).
#[derive(Clone)]
pub(crate) struct RestApiTool {
    /// The exposed name: the operation's name, or `<prefix>_<name>`.
    name: String,
    declaration: FunctionDeclaration,
    operation: Arc<Operation>,
    shared: Arc<Shared>,
}

impl RestApiTool {
    pub(crate) fn new(operation: Operation, shared: Arc<Shared>) -> Self {
        let declaration = FunctionDeclaration {
            name: operation.name.clone(),
            description: operation.description.clone(),
            parameters_json_schema: Some(operation.parameters_schema()),
            ..Default::default()
        };
        Self {
            name: operation.name.clone(),
            declaration,
            operation: Arc::new(operation),
            shared,
        }
    }

    /// The operation's (unprefixed) name.
    pub(crate) fn original_name(&self) -> &str {
        &self.operation.name
    }

    /// Exposes the tool under `name`; the declaration takes it too.
    pub(crate) fn expose_as(&mut self, name: String) {
        self.declaration.name.clone_from(&name);
        self.name = name;
    }

    /// Whether a call with `args` needs confirmation, before any confirmation is given.
    fn confirmation_required(&self, args: &Value) -> bool {
        match &self.shared.confirmation_provider {
            Some(provider) => provider(&self.name, args),
            None => match self.shared.require_confirmation {
                RequireConfirmation::Mutating => self.operation.mutating,
                RequireConfirmation::Always => true,
                RequireConfirmation::Never => false,
            },
        }
    }

    /// Sends the request and shapes the response into `{status, body[, truncated]}`.
    async fn exchange(&self, ctx: &Context, parts: Parts) -> Result<Map<String, Value>, Error> {
        let client = match &self.shared.auth {
            Some(provider) => gadk_auth::client(
                self.shared.client.clone(),
                provider.clone(),
                AuthContext::from(ctx),
            ),
            None => reqwest_middleware::ClientBuilder::new(self.shared.client.clone()).build(),
        };
        let mut request = client.request(self.operation.method.clone(), parts.url);
        for (name, value) in parts.headers.iter().chain(&self.shared.headers) {
            request = request.header(name.as_str(), value.as_str());
        }
        if let Some(body) = &parts.body {
            request = request.json(body);
        }
        let mut response = request
            .send()
            .await
            .map_err(|err| self.request_error(Box::new(err)))?;
        let status = response.status().as_u16();
        let json = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(is_json);
        let max = self.shared.max_response_bytes;
        let mut bytes = Vec::new();
        let mut truncated = false;
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|err| self.request_error(Box::new(err)))?
        {
            let room = max - bytes.len();
            if chunk.len() > room {
                bytes.extend_from_slice(&chunk[..room]);
                truncated = true;
                break;
            }
            bytes.extend_from_slice(&chunk);
        }
        let text = || Value::String(String::from_utf8_lossy(&bytes).into_owned());
        let body = if truncated {
            text()
        } else if bytes.is_empty() {
            Value::Null
        } else if json {
            serde_json::from_slice(&bytes).unwrap_or_else(|_| text())
        } else {
            text()
        };
        let mut result = Map::new();
        result.insert("status".to_string(), Value::from(status));
        result.insert("body".to_string(), body);
        if truncated {
            result.insert("truncated".to_string(), Value::Bool(true));
        }
        Ok(result)
    }

    fn request_error(&self, source: Box<dyn std::error::Error + Send + Sync>) -> Error {
        Error::Request {
            tool: self.name.clone(),
            source,
        }
    }
}

#[async_trait::async_trait]
impl Tool for RestApiTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.operation.description
    }

    /// The declaration: name, description and `parameters_json_schema` (the operation's
    /// parameters and `body`).
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
impl RunnableTool for RestApiTool {
    /// Checks confirmation, builds the request from `args`, sends it (racing the timeout and
    /// the invocation's cancellation) and returns `{status, body[, truncated]}`.
    async fn run(&self, ctx: Context, args: Value) -> Result<Map<String, Value>, ToolError> {
        match ctx.tool_confirmation() {
            Some(confirmation) => {
                if !confirmation.confirmed {
                    return Err(ToolError::ConfirmationRejected);
                }
            }
            None => {
                if self.confirmation_required(&args) {
                    // `request_confirmation` also sets `skip_summarization`.
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

        let parts = request::build(
            &self.operation,
            &self.shared.base_url,
            self.shared.query_api_key.as_ref(),
            &args,
        )?;
        let token = ctx.cancellation_token().clone();
        let outcome = tokio::select! {
            biased;
            () = token.cancelled() => Err(Error::Cancelled { tool: self.name.clone() }),
            result = tokio::time::timeout(self.shared.timeout, self.exchange(&ctx, parts)) => {
                result.unwrap_or_else(|_| Err(Error::Timeout { tool: self.name.clone() }))
            }
        };
        outcome.map_err(|err| ToolError::Execution(Box::new(err)))
    }
}
