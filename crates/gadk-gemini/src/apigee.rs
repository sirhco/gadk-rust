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

//! Gemini through an Apigee proxy (port of ADK-Go `model/apigee`).
//!
//! Placeholder written by Task 1 of the 3a plan; Task 7 implements it.

use std::fmt;
use std::sync::Arc;

use futures::stream::BoxStream;
use gadk_model::{Backend, HttpClient, Llm, LlmRequest, LlmResponse, ModelError};
use gadk_platform::env::{EnvProvider, ProcessEnv};

use crate::error::GeminiError;

/// Configuration for [`ApigeeModel::new`] (port of Go `apigee.Config` and its options).
#[derive(Clone)]
pub struct ApigeeConfig {
    /// Proxy URL (Go `WithProxyURL`). When `None` or empty, `APIGEE_PROXY_URL` is read from
    /// [`env`](Self::env).
    pub proxy_url: Option<String>,
    /// Headers sent with every request (Go `WithCustomHeaders`).
    pub custom_headers: http::HeaderMap,
    /// Custom HTTP client (Go `WithHTTPClient`).
    pub http_client: Option<Arc<dyn HttpClient>>,
    /// Environment variable source (`APIGEE_PROXY_URL`, `GOOGLE_GENAI_USE_VERTEXAI`,
    /// `GOOGLE_CLOUD_PROJECT`, `GOOGLE_CLOUD_LOCATION`, and the Gemini client's variables).
    pub env: Arc<dyn EnvProvider>,
}

impl Default for ApigeeConfig {
    fn default() -> Self {
        Self {
            proxy_url: None,
            custom_headers: http::HeaderMap::new(),
            http_client: None,
            env: Arc::new(ProcessEnv),
        }
    }
}

impl fmt::Debug for ApigeeConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ApigeeConfig")
            .field("proxy_url", &self.proxy_url)
            .field(
                "custom_headers",
                &self.custom_headers.keys().collect::<Vec<_>>(),
            )
            .field("http_client", &self.http_client.is_some())
            .finish_non_exhaustive()
    }
}

/// The parts of an Apigee model name (Go `modelInfo`).
#[doc(hidden)]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelInfo {
    /// The Gemini model ID.
    pub model_id: String,
    /// The API version (`vN…`), or `""`.
    pub api_version: String,
    /// Whether the model is served by Vertex AI.
    pub is_vertex_ai: bool,
}

/// Go `parseModelName`: `apigee/[vertex_ai/|gemini/][vN/]<model>`, with
/// `GOOGLE_GENAI_USE_VERTEXAI` read through `env`; Go's `invalid model string: <name>` as
/// [`GeminiError::Config`]. Hidden: public only for the parity test.
#[doc(hidden)]
#[allow(unused_variables, clippy::todo)]
pub fn parse_model_name(model_name: &str, env: &dyn EnvProvider) -> Result<ModelInfo, GeminiError> {
    todo!("Task 7")
}

/// A Gemini model reached through an Apigee proxy (port of Go `apigeeModel`): it configures a
/// [`GeminiModel`](crate::GeminiModel) and delegates every request to it.
pub struct ApigeeModel {
    #[allow(dead_code)] // Task 7
    name: String,
}

#[allow(unused_variables, clippy::todo)]
impl ApigeeModel {
    /// Port of Go `apigee.NewModel`: parses `apigee/[vertex_ai|gemini/][vN/]<model>`,
    /// resolves the proxy URL and the Vertex environment, and builds the delegate Gemini model.
    pub async fn new(name: &str, cfg: ApigeeConfig) -> Result<ApigeeModel, GeminiError> {
        todo!("Task 7")
    }
}

#[allow(unused_variables, clippy::todo)]
impl Llm for ApigeeModel {
    fn name(&self) -> &str {
        todo!("Task 7")
    }

    fn generate_content(
        &self,
        req: LlmRequest,
        stream: bool,
    ) -> BoxStream<'static, Result<LlmResponse, ModelError>> {
        todo!("Task 7")
    }

    fn google_variant(&self) -> Option<Backend> {
        todo!("Task 7")
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}
