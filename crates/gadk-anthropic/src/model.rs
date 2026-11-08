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

//! The Claude model (port of ADK-Go `anthropicmodel.go` and `request.go`
//! `buildMessageParams`).
//!
//! Placeholder signatures written by Task 1; Task 8 implements them.

use std::sync::Arc;
use std::time::Duration;

use futures::stream::BoxStream;
use gadk_model::{Llm, LlmRequest, LlmResponse, ModelError};
use gadk_platform::env::EnvProvider;
use google_cloud_auth::credentials::Credentials;

use crate::client::Transport;
use crate::config::{Backend, ClientConfig, Effort, PromptCacheConfig};
use crate::error::AnthropicError;
use crate::wire::MessageNewParams;

/// OAuth scope requested for Application Default Credentials (Go `cloudPlatformScope`).
pub(crate) const CLOUD_PLATFORM_SCOPE: &str = "https://www.googleapis.com/auth/cloud-platform";

/// A Claude model on the Anthropic Messages API or Vertex AI (Go `anthropicModel`).
///
/// Build it with [`AnthropicModel::new`]; it implements [`Llm`]. Requests never mutate the
/// caller's [`LlmRequest`], and one model serves concurrent requests.
pub struct AnthropicModel {
    name: String,
    model_id: String,
    backend: Backend,
    project: String,
    location: String,
    max_tokens: u32,
    effort: Option<Effort>,
    prompt_cache: Option<PromptCacheConfig>,
    transport: Arc<Transport>,
}

/// What [`AnthropicModel::build_message_params`] returns (Go's `params, reqOpts,
/// unwrapOutput`).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct BuiltParams {
    /// The Messages params.
    pub(crate) params: MessageNewParams,
    /// The resolved model ID (the Vertex URL's model segment).
    pub(crate) model_id: String,
    /// The per-attempt timeout (`LlmRequest.http_options.timeout` when > 0).
    pub(crate) timeout: Option<Duration>,
    /// Whether text responses must be unwrapped from `{"result": …}`.
    pub(crate) unwrap_output: bool,
}

#[allow(unused_variables, clippy::todo)]
impl AnthropicModel {
    /// Port of Go `NewModel`: validates `name` and `cfg` in Go's order with Go's error texts,
    /// resolves credentials (env, ADC) and the endpoint, and builds the model.
    #[allow(clippy::new_ret_no_self)] // mirrors Go NewModel, which returns model.LLM
    pub async fn new(name: &str, cfg: ClientConfig) -> Result<Arc<dyn Llm>, AnthropicError> {
        Ok(Arc::new(Self::new_concrete(name, cfg).await?))
    }

    /// [`new`](Self::new), returning the concrete type.
    pub async fn new_concrete(name: &str, cfg: ClientConfig) -> Result<Self, AnthropicError> {
        todo!("Task 8")
    }

    /// Go `resolveModelID`: the configured model ID for an empty `req_model`, else
    /// `req_model` parsed, with Go's backend, project and location mismatch errors.
    pub(crate) fn resolve_model_id(&self, req_model: &str) -> Result<String, AnthropicError> {
        todo!("Task 8")
    }

    /// Go `buildMessageParams`: model ID, config validation, max tokens, system blocks, tools
    /// and choice, messages, cache breakpoints, output config, thinking, sampling parameters
    /// (omitted with Go's `omitting sampling parameters when thinking or effort is active`
    /// warning, fields `backend` and `model`, when thinking or effort is active), stop
    /// sequences, and the per-attempt timeout.
    pub(crate) fn build_message_params(
        &self,
        req: &LlmRequest,
    ) -> Result<BuiltParams, AnthropicError> {
        todo!("Task 8")
    }
}

/// Go `resolveVertexCredentials`: `cfg.credentials`, else Application Default Credentials with
/// [`CLOUD_PLATFORM_SCOPE`] (see [`adc_credentials`]).
#[allow(unused_variables, clippy::todo)]
pub(crate) fn resolve_vertex_credentials(
    cfg: &ClientConfig,
) -> Result<Credentials, AnthropicError> {
    todo!("Task 8")
}

/// Application Default Credentials read through `env`: `GOOGLE_APPLICATION_CREDENTIALS`'s file
/// dispatched on its `"type"` to google-cloud-auth's per-type builders (as gadk-auth's
/// `adc_token_source` does), else google-cloud-auth's default builder. Any failure is Go's
/// `ErrInvalidConfig` + `ErrAuthentication` text `failed to find Google Application Default
/// Credentials for Vertex AI`.
#[allow(unused_variables, clippy::todo)]
pub(crate) fn adc_credentials(env: &dyn EnvProvider) -> Result<Credentials, AnthropicError> {
    todo!("Task 8")
}

#[allow(unused_variables, clippy::todo)]
impl Llm for AnthropicModel {
    /// The name given to [`AnthropicModel::new`], trimmed (Go `Name`).
    fn name(&self) -> &str {
        todo!("Task 8")
    }

    /// Go `GenerateContent`: params (`build_message_params`), then one final response
    /// (non-streaming) or partials and a final response (streaming), every error as
    /// `ModelError::Other(AnthropicError)`. Go's nil-request check has no counterpart
    /// (`LlmRequest` is a value). The request is sent on the stream's first poll; dropping the
    /// stream cancels it.
    fn generate_content(
        &self,
        req: LlmRequest,
        stream: bool,
    ) -> BoxStream<'static, Result<LlmResponse, ModelError>> {
        todo!("Task 8")
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}
